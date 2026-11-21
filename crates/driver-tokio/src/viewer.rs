//! Serving viewers: CAF's side of the viewer wire
//! ([`caf_core::viewer`]), for a viewer at the other end of a WebSocket,
//! a socket or a pipe.
//!
//! The host owns the transport: it reads the viewer's messages, decodes
//! them (JSON for a web page, MessagePack in named mode for a Rust
//! viewer), and hands them to [`serve`] on one channel; it takes
//! [`ToViewer`]s from the other, encodes them and sends them on.
//!
//! ```ignore
//! let rules = Arc::new(ViewerRules::default()); // viewers may not open chats
//! let (to_caf, from) = mpsc::channel(16);
//! let (to, mut to_viewer) = mpsc::channel(64);
//! tokio::spawn(serve(caf.clone(), rules, from, to));
//! // …then pass each decoded message to `to_caf`, and send on each one
//! // `to_viewer` gives, encoded.
//! ```

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use caf_core::OpenChat;
use caf_core::agent::{AgentStatus, SignInState};
use caf_core::chat::AppItem;
use caf_core::ids::AgentName;
use caf_core::ids::{ChangeNo, ChatId};
use caf_core::viewer::{AgentAction, FromViewer, OpenRequest, Outcome, ToViewer, VERSIONS};
use tokio::sync::mpsc;

use crate::caf::{AgentsWatch, Caf, ChatsWatch, Error, Update, Watch, WeakCaf};

/// Turns a viewer's request to open a chat into one, or refuses it: see
/// [`ViewerRules::open`].
pub type Opener = dyn Fn(&OpenRequest) -> Option<OpenChat> + Send + Sync;

/// What viewers may do. One Caf is one circle of trust: a viewer may
/// watch any chat in it. A host whose users mustn't see each other's
/// chats runs a Caf for each.
#[non_exhaustive]
pub struct ViewerRules {
    /// Turns a viewer's request to open a chat into one, or `None` to
    /// refuse it with [`Outcome::NotAllowed`]. It's the host's to decide:
    /// it chooses the agent's tools, and may limit the folders. The
    /// default refuses every request, so viewers can only watch chats the
    /// host opened.
    ///
    /// It's called on the viewer's task, and should be quick: a host that
    /// needs I/O to decide, such as asking its user, decides beforehand
    /// (an allowed list of folders, say) or refuses.
    ///
    /// [`Outcome::NotAllowed`]: caf_core::viewer::Outcome::NotAllowed
    pub open: Box<Opener>,
    /// Whether viewers may send, stop, answer and close. `true` by
    /// default; `false` makes them watch only.
    pub act: bool,
    /// How many chats one viewer may watch at once: 256 by default. A
    /// `Watch` for one more is answered with [`ToViewer::Ended`] at once.
    /// Watching a chat over again doesn't count, and a chat that has
    /// ended, or was unwatched, makes room for another.
    pub max_watches: usize,
    /// Whether viewers may install agents, and sign in and out of them.
    /// `false` by default: an install is hundreds of megabytes, and a
    /// sign-in decides whose account the agent uses, so a host opts in.
    /// Refused requests get [`Outcome::NotAllowed`].
    ///
    /// [`Outcome::NotAllowed`]: caf_core::viewer::Outcome::NotAllowed
    pub agents: bool,
}

impl Default for ViewerRules {
    fn default() -> Self {
        Self {
            open: Box::new(|_| None),
            act: true,
            max_watches: 256,
            agents: false,
        }
    }
}

/// Runs the viewer wire for one viewer, until the viewer's side closes
/// `from` or drops `to`'s receiver, the viewer breaks the wire's rules
/// (such as not starting with `Hello`), or CAF shuts down. CAF going on
/// without this viewer is unaffected either way, and a viewer doesn't
/// keep CAF running: once the host has dropped every [`Caf`] and chat
/// handle, CAF shuts down as usual, and its viewers see it end.
///
/// `to` should be bounded. While it's full, no more of a chat's changes
/// are taken; once there's room, a viewer that fell far behind gets the
/// chat whole again rather than every change it missed. So what waits
/// for a slow viewer is what's in `to`: up to its capacity in messages,
/// each up to a whole chat. Meanwhile `serve` takes at most one more of
/// the viewer's messages, which waits for room for its answer.
///
/// When CAF shuts down, `serve` tells the viewer each watched chat has
/// ended, then ends: that needs room in `to`, so a viewer that has
/// stopped reading keeps it waiting until it closes `from` or drops
/// `to`'s receiver. A closed `from` is noticed only once the message
/// already taken from it, if any, has been answered, so a host whose
/// transport breaks should drop `to`'s receiver too.
///
/// Right after the welcome comes [`ToViewer::Agents`], every agent's
/// status, and again each time any of it changes: only the latest, if it
/// changes faster than the viewer reads. A request to install an agent,
/// or sign in or out of one ([`FromViewer::Agent`]), is done only if
/// [`ViewerRules::agents`] allows it, and otherwise answered
/// [`Outcome::NotAllowed`]. A viewer it doesn't allow sees a sign-in under
/// way without its link or code: whoever opens the link, or types the
/// code, chooses whose account the agent signs in to.
///
/// Every request gets exactly one [`ToViewer::Done`], unless the link
/// ends first: one made while CAF shuts down is answered
/// [`Outcome::Gone`].
///
/// `serve` takes messages already decoded, so it can't limit their size:
/// the host's transport should refuse a message bigger than it means to
/// take, before decoding it.
pub async fn serve<A: AppItem>(
    caf: Caf<A>,
    rules: Arc<ViewerRules>,
    mut from: mpsc::Receiver<FromViewer>,
    to: mpsc::Sender<ToViewer<A>>,
) {
    // A viewer doesn't keep CAF running.
    let caf = {
        let weak = caf.downgrade();
        drop(caf);
        weak
    };
    let Some(version) = hello(&caf, &mut from, &to).await else {
        return;
    };
    let mut chats = caf.watch_chats();
    let Some(open) = chats.next().await else {
        return;
    };
    let mut agents = caf.watch_agents();
    let Some(statuses) = agents.next().await else {
        return;
    };
    let welcome = ToViewer::Welcome {
        version,
        agents: caf.agents().to_vec(),
        chats: open,
        run: caf.run().to_owned(),
    };
    let statuses = ToViewer::Agents(for_viewer(statuses, &rules));
    if to.send(welcome).await.is_err() || to.send(statuses).await.is_err() {
        return;
    }
    let link = Link {
        caf,
        rules,
        chats,
        agents,
        gone: false,
        watches: Watches::default(),
    };
    link.run(from, &to).await;
}

/// The handshake: the version both speak, or `None` if the link ends.
async fn hello<A: AppItem>(
    caf: &WeakCaf<A>,
    from: &mut mpsc::Receiver<FromViewer>,
    to: &mpsc::Sender<ToViewer<A>>,
) -> Option<u32> {
    let first = tokio::select! {
        first = from.recv() => first?,
        () = to.closed() => return None,
        () = caf.ended() => return None,
    };
    // Anything else first ends the link.
    let FromViewer::Hello { min, max } = first else {
        return None;
    };
    let newest = max.min(*VERSIONS.end());
    if newest < min.max(*VERSIONS.start()) {
        let none = ToViewer::NoCommonVersion {
            min: *VERSIONS.start(),
            max: *VERSIONS.end(),
        };
        let _ = to.send(none).await;
        return None;
    }
    Some(newest)
}

/// One viewer's link, after the handshake.
struct Link<A: AppItem> {
    caf: WeakCaf<A>,
    rules: Arc<ViewerRules>,
    chats: ChatsWatch,
    agents: AgentsWatch,
    /// CAF has shut down: the link ends once every watch has.
    gone: bool,
    watches: Watches<A>,
}

impl<A: AppItem> Link<A> {
    async fn run(mut self, mut from: mpsc::Receiver<FromViewer>, to: &mpsc::Sender<ToViewer<A>>) {
        // A message from the viewer taken while there was no room for its
        // answer.
        let mut waiting = None;
        loop {
            if self.gone && self.watches.is_empty() {
                return;
            }
            // Room for one message first, so nothing is taken from a
            // watch while the viewer can't take it: the watch falls
            // behind instead, and catches up with a picture. Meanwhile,
            // one message is taken from the viewer, so a closed `from` is
            // noticed.
            let room = tokio::select! {
                room = to.reserve() => match room {
                    Ok(room) => room,
                    Err(_) => return,
                },
                message = from.recv(), if waiting.is_none() => match message {
                    Some(message) => {
                        waiting = Some(message);
                        continue;
                    }
                    None => return,
                },
                // With no watched chat to say has ended, the link ends
                // with CAF.
                () = self.caf.ended(), if self.watches.is_empty() => return,
            };
            if let Some(message) = waiting.take() {
                self.take(message, room).await;
                continue;
            }
            tokio::select! {
                () = to.closed() => return,
                message = from.recv() => match message {
                    Some(message) => self.take(message, room).await,
                    None => return,
                },
                chats = self.chats.next(), if !self.gone => match chats {
                    Some(chats) => room.send(ToViewer::Chats(chats)),
                    None => self.gone = true,
                },
                statuses = self.agents.next(), if !self.gone => match statuses {
                    Some(statuses) => room.send(ToViewer::Agents(for_viewer(statuses, &self.rules))),
                    None => self.gone = true,
                },
                message = self.watches.next() => room.send(message),
            }
        }
    }

    /// Does what the viewer asked, saying how it went in `room`.
    async fn take(&mut self, message: FromViewer, room: mpsc::Permit<'_, ToViewer<A>>) {
        // Only these are held while CAF answers, so the link needn't be
        // shared between threads.
        let caf = &self.caf;
        let rules = &*self.rules;
        let (request, outcome) = match message {
            FromViewer::Watch { chat, since } => {
                if !self.watches.watching(chat) && self.watches.len() >= rules.max_watches {
                    room.send(ToViewer::Ended { chat });
                } else {
                    self.watches.watch(caf, chat, since);
                }
                return;
            }
            FromViewer::Unwatch { chat } => {
                self.watches.unwatch(chat);
                return;
            }
            FromViewer::Open { request, open } => (request, opened(caf, rules, &open).await),
            FromViewer::Send {
                request,
                chat,
                message,
            } => {
                let outcome = act(caf, rules, |caf| async move {
                    caf.chat(chat).send(message).await.map(Outcome::Sent)
                })
                .await;
                (request, outcome)
            }
            FromViewer::Stop { request, chat } => {
                let outcome = act(caf, rules, |caf| async move {
                    caf.chat(chat).stop().await.map(|()| Outcome::Done)
                })
                .await;
                (request, outcome)
            }
            FromViewer::Answer {
                request,
                question,
                answer,
            } => {
                let outcome = act(caf, rules, |caf| async move {
                    (caf.answer(question, answer).await).map(|()| Outcome::Done)
                })
                .await;
                (request, outcome)
            }
            FromViewer::Close { request, chat } => {
                let outcome = act(caf, rules, |caf| async move {
                    caf.chat(chat).close().await.map(|()| Outcome::Done)
                })
                .await;
                (request, outcome)
            }
            FromViewer::Agent {
                request,
                agent,
                action,
            } => (request, about(caf, rules, &agent, action).await),
            // A second Hello, or a message of a later version: there's
            // nothing to do.
            _ => return,
        };
        room.send(ToViewer::Done { request, outcome });
    }
}

/// Asks the host whether to open the chat, and opens it if so.
async fn opened<A: AppItem>(
    caf: &WeakCaf<A>,
    rules: &ViewerRules,
    request: &OpenRequest,
) -> Outcome {
    let opener = &rules.open;
    let open = match std::panic::catch_unwind(AssertUnwindSafe(|| opener(request))) {
        Ok(Some(open)) => open,
        Ok(None) => return Outcome::NotAllowed,
        Err(_) => {
            log::warn!(target: "caf", "the host's opener panicked on a viewer's request: not allowed");
            return Outcome::NotAllowed;
        }
    };
    let Some(caf) = caf.upgrade() else {
        return Outcome::Gone;
    };
    outcome(
        caf.open_chat(open)
            .await
            .map(|chat| Outcome::Opened(chat.id)),
    )
}

/// Sends, stops, answers or closes, if viewers may.
async fn act<A: AppItem, F: Future<Output = Result<Outcome, Error>>>(
    caf: &WeakCaf<A>,
    rules: &ViewerRules,
    act: impl FnOnce(Caf<A>) -> F,
) -> Outcome {
    if !rules.act {
        return Outcome::NotAllowed;
    }
    let Some(caf) = caf.upgrade() else {
        return Outcome::Gone;
    };
    outcome(act(caf).await)
}

/// Installs an agent, or signs in or out of it, if viewers may.
async fn about<A: AppItem>(
    caf: &WeakCaf<A>,
    rules: &ViewerRules,
    agent: &AgentName,
    action: AgentAction,
) -> Outcome {
    if !rules.agents {
        return Outcome::NotAllowed;
    }
    let Some(caf) = caf.upgrade() else {
        return Outcome::Gone;
    };
    let done = match action {
        AgentAction::Install => caf.install(agent).await,
        AgentAction::StopInstall => caf.stop_install(agent).await,
        AgentAction::SignIn => caf.sign_in(agent).await,
        AgentAction::Paste(text) => caf.paste(agent, text).await,
        AgentAction::StopSignIn => caf.stop_sign_in(agent).await,
        AgentAction::SignOut => caf.sign_out(agent).await,
        // One added in a later version of the wire, which this CAF can't
        // do.
        _ => return Outcome::NotAllowed,
    };
    outcome(done.map(|()| Outcome::Done))
}

/// How a request went, as the wire says it.
fn outcome(result: Result<Outcome, Error>) -> Outcome {
    match result {
        Ok(outcome) => outcome,
        Err(Error::Refused(refused)) => Outcome::Refused(refused),
        Err(_) => Outcome::Gone,
    }
}

/// A watch's next update, owning the watch while it waits, so it can be
/// kept from one turn of the loop to the next.
type Next<A> = Pin<Box<dyn Future<Output = (Watch<A>, Option<Update<A>>)> + Send>>;

fn next_of<A: AppItem>(mut watch: Watch<A>) -> Next<A> {
    Box::pin(async move {
        let update = watch.next().await;
        (watch, update)
    })
}

/// The chats a viewer watches, each waiting for its next update.
struct Watches<A: AppItem> {
    each: Vec<Watched<A>>,
    /// Where the next look starts, so no chat goes unheard.
    turn: usize,
}

/// One watched chat.
struct Watched<A: AppItem> {
    chat: ChatId,
    next: Next<A>,
    /// Where the viewer asked to start from.
    since: Option<ChangeNo>,
    /// Something has been sent for it since the viewer asked.
    sent: bool,
}

impl<A: AppItem> Watched<A> {
    fn new(caf: &WeakCaf<A>, chat: ChatId, since: Option<ChangeNo>) -> Self {
        Self {
            chat,
            next: next_of(caf.watch(chat, since)),
            since,
            sent: false,
        }
    }
}

impl<A: AppItem> Default for Watches<A> {
    fn default() -> Self {
        Self {
            each: Vec::new(),
            turn: 0,
        }
    }
}

impl<A: AppItem> Watches<A> {
    fn is_empty(&self) -> bool {
        self.each.is_empty()
    }

    fn len(&self) -> usize {
        self.each.len()
    }

    fn watching(&self, chat: ChatId) -> bool {
        self.each.iter().any(|each| each.chat == chat)
    }

    /// Watches `chat` from `since`, in place of any watch it had.
    ///
    /// A watch from the same place that hasn't sent anything yet may be
    /// reading the whole chat, so it's kept rather than started over: a
    /// viewer asking again and again costs no more than asking once. One
    /// from somewhere else is started over; CAF doesn't answer a read the
    /// old watch was waiting for.
    fn watch(&mut self, caf: &WeakCaf<A>, chat: ChatId, since: Option<ChangeNo>) {
        match self.each.iter_mut().find(|each| each.chat == chat) {
            Some(each) if !each.sent && each.since == since => {}
            Some(each) => *each = Watched::new(caf, chat, since),
            None => self.each.push(Watched::new(caf, chat, since)),
        }
    }

    fn unwatch(&mut self, chat: ChatId) {
        self.each.retain(|each| each.chat != chat);
    }

    /// The next message from any watched chat: an update, or that it has
    /// ended. Never resolves while nothing is watched. Cancel-safe.
    async fn next(&mut self) -> ToViewer<A> {
        std::future::poll_fn(|cx| {
            let count = self.each.len();
            for look in 0..count {
                let at = (self.turn + look) % count;
                let each = &mut self.each[at];
                let chat = each.chat;
                let Poll::Ready((watch, update)) = each.next.as_mut().poll(cx) else {
                    continue;
                };
                self.turn = at + 1;
                let Some(update) = update else {
                    drop(self.each.swap_remove(at));
                    return Poll::Ready(ToViewer::Ended { chat });
                };
                let message = match update {
                    Update::Picture(whole) => ToViewer::Picture {
                        chat: whole,
                        latest: watch.latest().unwrap_or(ChangeNo(0)),
                    },
                    Update::Changes(changes) => ToViewer::Changes { chat, changes },
                };
                each.sent = true;
                each.next = next_of(watch);
                return Poll::Ready(message);
            }
            Poll::Pending
        })
        .await
    }
}

/// Agents' statuses as a viewer sees them: a sign-in's link and code only
/// if the viewer may act on agents. Whoever opens the link, or types the
/// code on the provider's site, chooses whose account the agent signs in
/// to.
fn for_viewer(mut statuses: Vec<AgentStatus>, rules: &ViewerRules) -> Vec<AgentStatus> {
    if !rules.agents {
        for status in &mut statuses {
            if let SignInState::SigningIn(link) = &mut status.sign_in {
                link.url = None;
                link.code = None;
            }
        }
    }
    statuses
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One agent signing in, with its link and code.
    fn signing_in() -> Vec<AgentStatus> {
        serde_json::from_value(serde_json::json!([{
            "name": "claude",
            "install": { "type": "Host" },
            "sign_in": { "type": "SigningIn", "value": {
                "url": "https://claude.ai/oauth?state=s",
                "code": "ABCD-1234",
                "paste": true,
            } },
        }]))
        .expect("a status")
    }

    #[test]
    fn a_viewer_that_may_only_watch_never_sees_a_sign_ins_link_or_code() {
        let watching = for_viewer(signing_in(), &ViewerRules::default());
        let SignInState::SigningIn(link) = &watching[0].sign_in else {
            panic!("{watching:?}")
        };
        assert_eq!((&link.url, &link.code, link.paste), (&None, &None, true));
        let rules = ViewerRules {
            agents: true,
            ..ViewerRules::default()
        };
        assert_eq!(
            for_viewer(signing_in(), &rules),
            signing_in(),
            "one that may act sees both"
        );
    }
}
