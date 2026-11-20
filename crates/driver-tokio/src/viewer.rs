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
use caf_core::chat::AppItem;
use caf_core::ids::{ChangeNo, ChatId};
use caf_core::viewer::{FromViewer, OpenRequest, Outcome, ToViewer, VERSIONS};
use tokio::sync::mpsc;

use crate::caf::{Caf, ChatsWatch, Error, Update, Watch, WeakCaf};

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
}

impl Default for ViewerRules {
    fn default() -> Self {
        Self {
            open: Box::new(|_| None),
            act: true,
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
/// chat whole again rather than every change it missed. So a slow viewer
/// never makes CAF hold more for it than one chat's latest picture.
///
/// A request CAF can't take because it's shutting down gets no
/// [`ToViewer::Done`]: the link ends soon after, which tells the viewer.
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
    let welcome = ToViewer::Welcome {
        version,
        agents: caf.agents().to_vec(),
        chats: open,
    };
    if to.send(welcome).await.is_err() {
        return;
    }
    let link = Link {
        caf,
        rules,
        chats,
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
    /// CAF has shut down: the link ends once every watch has.
    gone: bool,
    watches: Watches<A>,
}

impl<A: AppItem> Link<A> {
    async fn run(mut self, mut from: mpsc::Receiver<FromViewer>, to: &mpsc::Sender<ToViewer<A>>) {
        loop {
            if self.gone && self.watches.is_empty() {
                return;
            }
            // Room for one message first, so nothing is taken from a
            // watch while the viewer can't take it: the watch falls
            // behind instead, and catches up with a picture.
            let Ok(room) = to.reserve().await else {
                return;
            };
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
                self.watches.watch(chat, caf.watch(chat, since));
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
            // A second Hello, or a message of a later version: there's
            // nothing to do.
            _ => return,
        };
        if let Some(outcome) = outcome {
            room.send(ToViewer::Done { request, outcome });
        }
    }
}

/// Asks the host whether to open the chat, and opens it if so. `None` if
/// CAF is shutting down.
async fn opened<A: AppItem>(
    caf: &WeakCaf<A>,
    rules: &ViewerRules,
    request: &OpenRequest,
) -> Option<Outcome> {
    let opener = &rules.open;
    let open = match std::panic::catch_unwind(AssertUnwindSafe(|| opener(request))) {
        Ok(Some(open)) => open,
        Ok(None) => return Some(Outcome::NotAllowed),
        Err(_) => {
            log::warn!(target: "caf", "the host's opener panicked on a viewer's request: not allowed");
            return Some(Outcome::NotAllowed);
        }
    };
    let caf = caf.upgrade()?;
    outcome(
        caf.open_chat(open)
            .await
            .map(|chat| Outcome::Opened(chat.id)),
    )
}

/// Sends, stops, answers or closes, if viewers may. `None` if CAF is
/// shutting down.
async fn act<A: AppItem, F: Future<Output = Result<Outcome, Error>>>(
    caf: &WeakCaf<A>,
    rules: &ViewerRules,
    act: impl FnOnce(Caf<A>) -> F,
) -> Option<Outcome> {
    if !rules.act {
        return Some(Outcome::NotAllowed);
    }
    outcome(act(caf.upgrade()?).await)
}

/// How a request went, as the wire says it, or `None` if CAF is gone.
fn outcome(result: Result<Outcome, Error>) -> Option<Outcome> {
    match result {
        Ok(outcome) => Some(outcome),
        Err(Error::Refused(refused)) => Some(Outcome::Refused(refused)),
        Err(_) => None,
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
    each: Vec<(ChatId, Next<A>)>,
    /// Where the next look starts, so no chat goes unheard.
    turn: usize,
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

    /// Watches `chat` with `watch`, in place of any watch it had.
    fn watch(&mut self, chat: ChatId, watch: Watch<A>) {
        self.unwatch(chat);
        self.each.push((chat, next_of(watch)));
    }

    fn unwatch(&mut self, chat: ChatId) {
        self.each.retain(|(each, _)| *each != chat);
    }

    /// The next message from any watched chat: an update, or that it has
    /// ended. Never resolves while nothing is watched. Cancel-safe.
    async fn next(&mut self) -> ToViewer<A> {
        std::future::poll_fn(|cx| {
            let count = self.each.len();
            for look in 0..count {
                let at = (self.turn + look) % count;
                let (chat, next) = &mut self.each[at];
                let chat = *chat;
                let Poll::Ready((watch, update)) = next.as_mut().poll(cx) else {
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
                self.each[at].1 = next_of(watch);
                return Poll::Ready(message);
            }
            Poll::Pending
        })
        .await
    }
}
