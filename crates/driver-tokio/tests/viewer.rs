//! Serving viewers, over in-memory channels, against the fake adapter.

mod common;

use std::collections::VecDeque;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;

use caf_core::agent::AgentStatus;
use caf_core::change::{Change, Numbered};
use caf_core::chat::{ChatState, ItemKind, Lost, TurnLife, UserMessage};
use caf_core::ids::{AgentName, ChangeNo, ChatId, QuestionId};
use caf_core::question::{Answer, OptionId};
use caf_core::settings::{KnownSettings, SettingKind, SettingValue};
use caf_core::time::Wall;
use caf_core::usage::{BadQuery, ModelPrice, PriceLayer, UsageQuery};
use caf_core::viewer::{
    AgentAction, ChatSummary, FromViewer, OpenRequest, Outcome, RequestNo, Rights, ToViewer,
};
use caf_core::{Chat, OpenChat, Refused, SettingChoice};
use caf_driver_tokio::viewer::{ViewerRules, serve};
use caf_driver_tokio::{Caf, Running};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use common::*;

/// One viewer, at the other end of `serve`.
struct Viewer {
    to: mpsc::Sender<FromViewer>,
    from: mpsc::Receiver<ToViewer>,
    served: JoinHandle<()>,
    /// What [`Viewer::done`] read past, for [`Viewer::next`] to give.
    skipped: VecDeque<ToViewer>,
    /// Agents' statuses heard after the welcome's: the readers pass over
    /// them, as they come whenever a status changes.
    statuses: VecDeque<Vec<AgentStatus>>,
}

impl Viewer {
    /// Connects a viewer, with room for `room` messages to it.
    fn connect(caf: &Caf, rules: ViewerRules, room: usize) -> Self {
        let (to, from_viewer) = mpsc::channel(16);
        let (to_viewer, from) = mpsc::channel(room);
        let served = tokio::spawn(serve(caf.clone(), Arc::new(rules), from_viewer, to_viewer));
        Self {
            to,
            from,
            served,
            skipped: VecDeque::new(),
            statuses: VecDeque::new(),
        }
    }

    /// Connects and says hello, returning the welcome's agents and chats.
    async fn welcomed(
        caf: &Caf,
        rules: ViewerRules,
        room: usize,
    ) -> (Self, Vec<AgentName>, Vec<ChatSummary>) {
        let mut viewer = Self::connect(caf, rules, room);
        viewer.send(FromViewer::Hello { min: 1, max: 1 }).await;
        match viewer.next().await {
            Some(ToViewer::Welcome {
                version: 1,
                agents,
                chats,
                run,
                ..
            }) => {
                assert_eq!(run, caf.run(), "the viewer hears CAF's run");
                // Every agent's status comes next, at once.
                match viewer.from.recv().await {
                    Some(ToViewer::Agents(statuses)) => {
                        let names: Vec<AgentName> =
                            statuses.iter().map(|status| status.name.clone()).collect();
                        assert_eq!(names, agents, "every agent's status, in order");
                        viewer.statuses.push_back(statuses);
                    }
                    other => panic!("the agents' statuses after the welcome, not {other:?}"),
                }
                (viewer, agents, chats)
            }
            other => panic!("a welcome, not {other:?}"),
        }
    }

    async fn send(&self, message: FromViewer) {
        self.to.send(message).await.expect("serve is still running");
    }

    /// Waits until `serve` has taken everything sent to it.
    async fn taken(&self) {
        let taken = tokio::time::timeout(PATIENCE, async {
            while self.to.capacity() < self.to.max_capacity() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(taken.is_ok(), "serve takes what it's sent");
    }

    /// The next message other than agents' statuses, or `None` once the
    /// link has ended.
    async fn next(&mut self) -> Option<ToViewer> {
        if let Some(skipped) = self.skipped.pop_front() {
            return Some(skipped);
        }
        loop {
            match self.received().await {
                Some(ToViewer::Agents(statuses)) => self.statuses.push_back(statuses),
                other => return other,
            }
        }
    }

    /// The next message on the link.
    async fn received(&mut self) -> Option<ToViewer> {
        tokio::time::timeout(PATIENCE, self.from.recv())
            .await
            .expect("a message, or the link's end, in good time")
    }

    /// Reads until the answer to `request`, and gives it. What came
    /// before it comes next.
    async fn done(&mut self, request: u64) -> Outcome {
        let mut before = VecDeque::new();
        loop {
            match self.next().await {
                Some(ToViewer::Done {
                    request: answered,
                    outcome,
                }) if answered == RequestNo(request) => {
                    before.append(&mut self.skipped);
                    self.skipped = before;
                    return outcome;
                }
                Some(other) => before.push_back(other),
                None => panic!("the link ended before request {request} was answered: {before:?}"),
            }
        }
    }

    /// Watches `chat`, and reads its picture.
    async fn watch(&mut self, chat: ChatId) -> (Chat, ChangeNo) {
        self.send(FromViewer::Watch { chat, since: None }).await;
        match self.next().await {
            Some(ToViewer::Picture {
                chat: whole,
                latest,
            }) if whole.id == chat => (whole, latest),
            other => panic!("a picture of the chat first, not {other:?}"),
        }
    }

    /// Reads `chat`'s changes until its last turn finishes, giving the
    /// words heard and the last change's number.
    async fn until_finished(&mut self, chat: ChatId) -> (String, ChangeNo) {
        let mut heard = String::new();
        loop {
            match self.next().await {
                Some(ToViewer::Changes {
                    chat: changed,
                    changes,
                }) if changed == chat => {
                    let finished = hear(&changes, &mut heard);
                    if finished {
                        return (heard, changes.last().unwrap().no);
                    }
                }
                Some(ToViewer::Picture { chat: whole, .. }) if whole.id == chat => {
                    panic!("a viewer that keeps up gets changes, not a picture")
                }
                Some(_) => {}
                None => panic!("the link ended; heard {heard:?}"),
            }
        }
    }

    /// The link ends, and `serve` with it.
    async fn ends(mut self) {
        while let Some(message) = self.next().await {
            assert!(
                matches!(
                    message,
                    ToViewer::Ended { .. }
                        | ToViewer::Picture { .. }
                        | ToViewer::Changes { .. }
                        | ToViewer::Chats(_)
                        | ToViewer::Agents(_)
                ),
                "only what was on its way before the end, not {message:?}"
            );
        }
        let served = tokio::time::timeout(PATIENCE, self.served).await;
        assert!(
            matches!(served, Ok(Ok(()))),
            "serve ends cleanly: {served:?}"
        );
    }
}

/// Adds the words in `changes` to `heard`, and says whether a turn
/// finished.
fn hear(changes: &[Numbered], heard: &mut String) -> bool {
    let mut finished = false;
    for numbered in changes {
        match &numbered.change {
            Change::ItemAdded { item, .. } => {
                if let ItemKind::Words(words) = &item.kind {
                    heard.push_str(words);
                }
            }
            Change::Appended { text, .. } => heard.push_str(text),
            Change::TurnLife {
                life: TurnLife::Finished,
                ..
            } => finished = true,
            _ => {}
        }
    }
    finished
}

/// Rules that let viewers open chats with any agent in any folder, as
/// they ask.
fn opening() -> ViewerRules {
    let mut rules = ViewerRules::default();
    rules.open = Box::new(|request| {
        let mut open = OpenChat::new(request.agent.0.clone(), request.cwd.clone());
        open.session = request.session.clone();
        Some(open)
    });
    rules
}

/// CAF with the fake running `steps` after its handshake, and a chat the
/// host opened.
async fn started(dir: &Dir, steps: &[Value]) -> (Caf, Running, caf_driver_tokio::ChatHandle) {
    let script = dir.script("script.jsonl", &after_handshake(steps));
    let (caf, running) = Caf::start(config_for(host(), vec![fake(dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    (caf, running, chat)
}

fn hello_there() -> Vec<Value> {
    vec![
        prompt(),
        say("s1", "Hello "),
        json!({"sleep": 50}),
        say("s1", "there"),
        end_turn(),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_welcome_has_the_agents_and_the_open_chats() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &[]).await;
    let mut viewer = Viewer::connect(&caf, ViewerRules::default(), 8);
    // A viewer that speaks later versions too gets the newest both know.
    viewer.send(FromViewer::Hello { min: 0, max: 7 }).await;
    let Some(ToViewer::Welcome {
        version,
        agents,
        chats,
        may,
        ..
    }) = viewer.next().await
    else {
        panic!("a welcome")
    };
    assert_eq!(version, 1);
    // What the host lets it do, by default: act on chats, but not agents.
    assert_eq!(may, Rights::new(true, false));
    assert_eq!(agents, [AgentName("fake".into())]);
    assert_eq!(caf.agents(), agents);
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].id, chat.id);
    assert_eq!(chats[0].agent, AgentName("fake".into()));
    assert_eq!(chats[0].cwd, dir.0);
    // A second hello is nothing to answer.
    viewer.send(FromViewer::Hello { min: 1, max: 1 }).await;
    viewer
        .send(FromViewer::Watch {
            chat: chat.id,
            since: None,
        })
        .await;
    assert!(matches!(
        viewer.next().await,
        Some(ToViewer::Picture { .. })
    ));
    shut_down(&caf, running).await;
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agents_come_right_after_the_welcome_while_chats_open_and_close() {
    let dir = Dir::new();
    let (caf, running, _chat) = started(&dir, &[]).await;
    // The open chats change all the while viewers connect, so a list of
    // them is always ready to go out.
    let churn = {
        let caf = caf.clone();
        let cwd = dir.0.clone();
        tokio::spawn(async move {
            for _ in 0..40 {
                if let Ok(chat) = caf.open_chat(OpenChat::new("fake", &cwd)).await {
                    let _ = chat.close().await;
                }
            }
        })
    };
    let viewers: Vec<_> = (0..40)
        .map(|_| {
            let caf = caf.clone();
            // `welcomed` checks the very next message after the welcome is
            // the agents' statuses.
            tokio::spawn(async move { Viewer::welcomed(&caf, ViewerRules::default(), 64).await })
        })
        .collect();
    for viewer in viewers {
        let (viewer, agents, _) = viewer
            .await
            .expect("each viewer is welcomed, then told the agents");
        assert_eq!(agents, [AgentName("fake".into())]);
        drop(viewer);
    }
    churn.await.unwrap();
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_welcome_says_what_the_host_lets_the_viewer_do() {
    let dir = Dir::new();
    let (caf, running, _chat) = started(&dir, &[]).await;
    for (act, agents, usage) in [
        (true, true, false),
        (false, true, false),
        (false, false, false),
        (true, false, true),
    ] {
        let mut rules = ViewerRules::default();
        rules.act = act;
        rules.agents = agents;
        rules.usage = usage;
        let mut viewer = Viewer::connect(&caf, rules, 8);
        viewer.send(FromViewer::Hello { min: 1, max: 1 }).await;
        let Some(ToViewer::Welcome { may, .. }) = viewer.next().await else {
            panic!("a welcome")
        };
        assert_eq!(may, Rights::new(act, agents).with_usage(usage));
    }
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anything_but_hello_first_ends_the_link() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &[]).await;
    let viewer = Viewer::connect(&caf, ViewerRules::default(), 8);
    viewer
        .send(FromViewer::Watch {
            chat: chat.id,
            since: None,
        })
        .await;
    viewer.ends().await;
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_common_version_says_so_and_ends_the_link() {
    let dir = Dir::new();
    let (caf, running, _chat) = started(&dir, &[]).await;
    for (min, max) in [(2, 9), (0, 0), (1, 0)] {
        let mut viewer = Viewer::connect(&caf, ViewerRules::default(), 8);
        viewer.send(FromViewer::Hello { min, max }).await;
        assert_eq!(
            viewer.next().await,
            Some(ToViewer::NoCommonVersion { min: 1, max: 1 }),
            "for {min}..={max}"
        );
        viewer.ends().await;
    }
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_gets_a_picture_then_the_turn_as_it_streams() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &hello_there()).await;
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    let (first, _) = viewer.watch(chat.id).await;
    assert!(first.turns.is_empty());
    viewer
        .send(FromViewer::Send {
            request: RequestNo(1),
            chat: chat.id,
            message: UserMessage::text("Say hello"),
        })
        .await;
    let outcome = viewer.done(1).await;
    let Outcome::Sent(turn) = outcome else {
        panic!("sent, not {outcome:?}")
    };
    let (heard, _) = viewer.until_finished(chat.id).await;
    assert_eq!(heard, "Hello there");
    let now = picture(&chat).await;
    assert_eq!(now.turns[0].id, turn);
    assert_eq!(words(&now.turns[0]), "Hello there");
    shut_down(&caf, running).await;
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_since_resumes_or_starts_over() {
    let dir = Dir::new();
    let mut steps = vec![prompt(), say("s1", "short"), end_turn(), prompt()];
    steps.extend((0..40).map(|n| say("s1", &format!("{n} "))));
    steps.push(end_turn());
    let script = dir.script("script.jsonl", &after_handshake(&steps));
    let mut host = host();
    host.limits.kept_changes = 16;
    let (caf, running) = Caf::start(config_for(host, vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    let (mut first, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 64).await;
    let (_, before) = first.watch(chat.id).await;
    chat.send(UserMessage::text("One")).await.unwrap();
    let (_, after) = first.until_finished(chat.id).await;
    assert!(after > before);

    // A viewer that has the chat up to `before` gets just what follows.
    let (mut again, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 64).await;
    again
        .send(FromViewer::Watch {
            chat: chat.id,
            since: Some(before),
        })
        .await;
    match again.next().await {
        Some(ToViewer::Changes {
            chat: changed,
            changes,
        }) => {
            assert_eq!(changed, chat.id);
            assert_eq!(changes[0].no, ChangeNo(before.0 + 1));
        }
        other => panic!("the changes after {before:?}, not {other:?}"),
    }

    // Once CAF no longer has them, it starts over with the chat whole.
    chat.send(UserMessage::text("Count")).await.unwrap();
    until(&chat, &mut watch, "the long turn", |chat| {
        chat.turns.len() == 2 && finished(chat)
    })
    .await;
    let (mut late, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 64).await;
    // Too old, and from a CAF that got further than this one ever did.
    for since in [after, ChangeNo(after.0 + 1_000_000)] {
        late.send(FromViewer::Watch {
            chat: chat.id,
            since: Some(since),
        })
        .await;
        match late.next().await {
            Some(ToViewer::Picture {
                chat: whole,
                latest,
            }) => {
                assert_eq!(whole.turns.len(), 2);
                assert!(latest > after, "{latest:?}");
            }
            other => panic!("a picture for {since:?}, not {other:?}"),
        }
    }
    shut_down(&caf, running).await;
    for viewer in [first, again, late] {
        viewer.ends().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_that_falls_behind_gets_a_picture() {
    let dir = Dir::new();
    let mut steps = vec![prompt()];
    steps.extend((0..40).map(|n| say("s1", &format!("{n} "))));
    steps.push(end_turn());
    let script = dir.script("script.jsonl", &after_handshake(&steps));
    let mut host = host();
    host.limits.kept_changes = 8;
    let (caf, running) = Caf::start(config_for(host, vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    // Room for one message only, and the viewer stops reading.
    let (mut slow, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 1).await;
    slow.watch(chat.id).await;
    chat.send(UserMessage::text("Count")).await.unwrap();
    let done = until(&chat, &mut watch, "the turn", finished).await;

    // What was waiting comes first, then the chat whole: not every change
    // it missed.
    let expected: String = (0..40).map(|n| format!("{n} ")).collect();
    let mut changes = 0;
    loop {
        match slow.next().await {
            Some(ToViewer::Changes { .. }) => changes += 1,
            Some(ToViewer::Picture { chat: whole, .. }) => {
                assert_eq!(whole, done);
                assert_eq!(words(&whole.turns[0]), expected);
                break;
            }
            other => panic!("changes, then a picture, not {other:?}"),
        }
    }
    assert!(changes <= 1, "{changes} lots of changes were held for it");
    shut_down(&caf, running).await;
    slow.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_is_refused_unless_the_host_allows_it() {
    let dir = Dir::new();
    let (caf, running, _chat) = started(&dir, &[]).await;
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    viewer
        .send(FromViewer::Open {
            request: RequestNo(1),
            open: OpenRequest::new("fake", &dir.0),
        })
        .await;
    assert_eq!(viewer.done(1).await, Outcome::NotAllowed);
    assert!(
        viewer.skipped.is_empty(),
        "nothing before it: {:?}",
        viewer.skipped
    );

    let (mut allowed, ..) = Viewer::welcomed(&caf, opening(), 8).await;
    allowed
        .send(FromViewer::Open {
            request: RequestNo(2),
            open: OpenRequest::new("fake", &dir.0),
        })
        .await;
    let outcome = allowed.done(2).await;
    let Outcome::Opened(opened) = outcome else {
        panic!("opened, not {outcome:?}")
    };
    // It's in the list that comes next.
    match allowed.next().await {
        Some(ToViewer::Chats(chats)) => {
            assert_eq!(chats.last().map(|chat| chat.id), Some(opened));
        }
        other => panic!("the new list, not {other:?}"),
    }
    // The core's refusals come through as they are.
    allowed
        .send(FromViewer::Open {
            request: RequestNo(3),
            open: OpenRequest::new("fake", "relative"),
        })
        .await;
    assert_eq!(
        allowed.done(3).await,
        Outcome::Refused(Refused::RelativeFolder)
    );
    allowed
        .send(FromViewer::Open {
            request: RequestNo(4),
            open: OpenRequest::new("nobody", &dir.0),
        })
        .await;
    assert_eq!(
        allowed.done(4).await,
        Outcome::Refused(Refused::NoSuchAgent)
    );

    // A host whose rule panics refuses, and the link goes on.
    let mut rules = ViewerRules::default();
    rules.open = Box::new(|_| panic!("a host with a bug"));
    let (mut buggy, ..) = Viewer::welcomed(&caf, rules, 8).await;
    buggy
        .send(FromViewer::Open {
            request: RequestNo(5),
            open: OpenRequest::new("fake", &dir.0),
        })
        .await;
    assert_eq!(buggy.done(5).await, Outcome::NotAllowed);
    buggy
        .send(FromViewer::Stop {
            request: RequestNo(6),
            chat: ChatId::from_number(999_999),
        })
        .await;
    assert_eq!(buggy.done(6).await, Outcome::Refused(Refused::NoSuchChat));
    shut_down(&caf, running).await;
    for viewer in [viewer, allowed, buggy] {
        viewer.ends().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_that_may_not_act_only_watches() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &hello_there()).await;
    let mut rules = opening();
    rules.act = false;
    let (mut viewer, ..) = Viewer::welcomed(&caf, rules, 8).await;
    viewer.watch(chat.id).await;
    let asks = [
        FromViewer::Send {
            request: RequestNo(1),
            chat: chat.id,
            message: UserMessage::text("Hi"),
        },
        FromViewer::Stop {
            request: RequestNo(2),
            chat: chat.id,
        },
        FromViewer::Answer {
            request: RequestNo(3),
            question: QuestionId(1),
            answer: Answer::Permission(OptionId("allow".into())),
        },
        FromViewer::Close {
            request: RequestNo(4),
            chat: chat.id,
        },
        FromViewer::Set {
            request: RequestNo(5),
            chat: chat.id,
            choice: SettingChoice::model("large"),
        },
    ];
    for (request, ask) in (1..).zip(asks) {
        viewer.send(ask).await;
        assert_eq!(viewer.done(request).await, Outcome::NotAllowed);
    }
    let now = picture(&chat).await;
    assert!(now.turns.is_empty());
    assert_ne!(now.state, ChatState::Closed);
    assert!(now.settings.chosen.is_empty(), "nothing chosen");
    // Opening is the host's rule's to decide, not `act`'s.
    viewer
        .send(FromViewer::Open {
            request: RequestNo(6),
            open: OpenRequest::new("fake", &dir.0),
        })
        .await;
    assert!(matches!(viewer.done(6).await, Outcome::Opened(_)));
    shut_down(&caf, running).await;
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_hears_of_chats_others_open_and_close() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &handshake("s1"));
    let (caf, running) = Caf::start(config_for(host(), vec![fake(&dir, &script)]));
    let (mut opener, ..) = Viewer::welcomed(&caf, opening(), 8).await;
    let (mut other, _, chats) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    assert!(chats.is_empty());
    opener
        .send(FromViewer::Open {
            request: RequestNo(1),
            open: OpenRequest::new("fake", &dir.0),
        })
        .await;
    let Outcome::Opened(first) = opener.done(1).await else {
        panic!("opened")
    };
    let ids = |chats: &[ChatSummary]| chats.iter().map(|chat| chat.id).collect::<Vec<_>>();
    match other.next().await {
        Some(ToViewer::Chats(chats)) => assert_eq!(ids(&chats), [first]),
        other => panic!("the new list, not {other:?}"),
    }
    let second = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    match other.next().await {
        Some(ToViewer::Chats(chats)) => assert_eq!(ids(&chats), [first, second.id]),
        other => panic!("the host's chat too, not {other:?}"),
    }
    opener
        .send(FromViewer::Close {
            request: RequestNo(2),
            chat: first,
        })
        .await;
    assert_eq!(opener.done(2).await, Outcome::Done);
    match other.next().await {
        Some(ToViewer::Chats(chats)) => assert_eq!(ids(&chats), [second.id]),
        other => panic!("the list without the closed chat, not {other:?}"),
    }
    // The kit's own watch on the list agrees.
    let mut list = caf.watch_chats();
    let now = list.next().await.unwrap();
    assert_eq!(ids(&now), [second.id]);
    shut_down(&caf, running).await;
    assert_eq!(list.next().await, None, "the list ends with CAF");
    opener.ends().await;
    other.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watched_chat_ends_when_it_closes() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &[]).await;
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    viewer.watch(chat.id).await;
    viewer
        .send(FromViewer::Close {
            request: RequestNo(9),
            chat: chat.id,
        })
        .await;
    assert_eq!(viewer.done(9).await, Outcome::Done);
    let mut heard = Vec::new();
    while !heard
        .iter()
        .any(|message| *message == ToViewer::Ended { chat: chat.id })
    {
        heard.push(viewer.next().await.expect("the chat's end"));
    }
    assert!(
        heard
            .iter()
            .any(|message| matches!(message, ToViewer::Chats(chats) if chats.is_empty())),
        "{heard:?}"
    );
    // Watching it again gives how it was left, and its end.
    viewer
        .send(FromViewer::Watch {
            chat: chat.id,
            since: None,
        })
        .await;
    match viewer.next().await {
        Some(ToViewer::Picture { chat: whole, .. }) => assert_eq!(whole.state, ChatState::Closed),
        other => panic!("the closed chat, not {other:?}"),
    }
    assert_eq!(viewer.next().await, Some(ToViewer::Ended { chat: chat.id }));
    // So does a chat that was never there.
    let never = ChatId::from_number(999_999);
    viewer
        .send(FromViewer::Watch {
            chat: never,
            since: None,
        })
        .await;
    assert_eq!(viewer.next().await, Some(ToViewer::Ended { chat: never }));
    shut_down(&caf, running).await;
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watched_chats_end_when_caf_shuts_down() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &hello_there()).await;
    // One viewer reads everything; the other has room for one message, and
    // stops reading once that's taken.
    let (mut keeping_up, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 64).await;
    let (mut behind, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 1).await;
    keeping_up.watch(chat.id).await;
    behind.watch(chat.id).await;
    chat.send(UserMessage::text("Say hello")).await.unwrap();
    keeping_up.until_finished(chat.id).await;
    shut_down(&caf, running).await;

    let mut latest = ChangeNo(0);
    loop {
        match keeping_up.next().await {
            Some(ToViewer::Changes { changes, .. }) => latest = changes.last().unwrap().no,
            // Its last read can come after CAF's task has ended, and then
            // it gets how CAF left the chat, as one that fell behind does.
            Some(ToViewer::Picture {
                chat: whole,
                latest: number,
            }) => {
                assert!(matches!(
                    whole.state,
                    ChatState::Lost {
                        reason: Lost::ShutDown
                    }
                ));
                assert!(number >= latest, "{number:?} is no older than {latest:?}");
                latest = number;
            }
            Some(ToViewer::Ended { chat: ended }) => {
                assert_eq!(ended, chat.id);
                break;
            }
            other => panic!("the last changes, then the end, not {other:?}"),
        }
    }
    assert_eq!(keeping_up.next().await, None, "the link ends with CAF");

    // The one behind gets how CAF left the chat, as far as it goes.
    let mut last = None;
    loop {
        match behind.next().await {
            Some(ToViewer::Changes { .. }) => {}
            Some(ToViewer::Picture {
                chat: whole,
                latest,
            }) => last = Some((whole, latest)),
            Some(ToViewer::Ended { .. }) => break,
            other => panic!("the last of the chat, then the end, not {other:?}"),
        }
    }
    let Some((whole, number)) = last else {
        panic!("a picture of how CAF left the chat")
    };
    assert!(matches!(
        whole.state,
        ChatState::Lost {
            reason: Lost::ShutDown
        }
    ));
    assert_eq!(
        number, latest,
        "the picture is numbered as of its last change"
    );
    assert_eq!(behind.next().await, None);
    keeping_up.ends().await;
    behind.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_leaving_mid_turn_harms_nothing() {
    let dir = Dir::new();
    let (caf, running, chat) = started(
        &dir,
        &[
            prompt(),
            say("s1", "one "),
            json!({"sleep": 300}),
            say("s1", "two"),
            end_turn(),
        ],
    )
    .await;
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    viewer.watch(chat.id).await;
    viewer
        .send(FromViewer::Send {
            request: RequestNo(1),
            chat: chat.id,
            message: UserMessage::text("Count"),
        })
        .await;
    viewer.done(1).await;
    // Gone once it has heard the first words.
    let mut heard = String::new();
    while heard.is_empty() {
        if let Some(ToViewer::Changes { changes, .. }) = viewer.next().await {
            hear(&changes, &mut heard);
        }
    }
    let Viewer {
        to, from, served, ..
    } = viewer;
    drop((to, from));
    assert!(
        matches!(tokio::time::timeout(PATIENCE, served).await, Ok(Ok(()))),
        "serve ends when its viewer goes"
    );
    let mut watch = chat.watch();
    let done = until(&chat, &mut watch, "the turn", finished).await;
    assert_eq!(words(&done.turns[0]), "one two");
    // Another viewer is served as ever.
    let (mut next, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    assert_eq!(next.watch(chat.id).await.0, done);
    shut_down(&caf, running).await;
    next.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_ends_when_either_channel_closes() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &[]).await;
    // The viewer stops sending, while it's watching.
    let (mut quiet, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    quiet.watch(chat.id).await;
    let Viewer {
        to, from, served, ..
    } = quiet;
    drop(to);
    assert!(matches!(
        tokio::time::timeout(PATIENCE, served).await,
        Ok(Ok(()))
    ));
    drop(from);
    // The viewer stops listening, while nothing changes.
    let (deaf, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    let Viewer {
        to, from, served, ..
    } = deaf;
    drop(from);
    assert!(matches!(
        tokio::time::timeout(PATIENCE, served).await,
        Ok(Ok(()))
    ));
    drop(to);
    // Before the handshake, too.
    let silent = Viewer::connect(&caf, ViewerRules::default(), 8);
    let Viewer {
        to, from, served, ..
    } = silent;
    drop(from);
    assert!(matches!(
        tokio::time::timeout(PATIENCE, served).await,
        Ok(Ok(()))
    ));
    drop(to);
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_viewers_follow_one_chat() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &hello_there()).await;
    let mut viewers = Vec::new();
    for _ in 0..8 {
        let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 4).await;
        viewer.watch(chat.id).await;
        viewers.push(viewer);
    }
    chat.send(UserMessage::text("Say hello")).await.unwrap();
    let mut heard = Vec::new();
    for viewer in &mut viewers {
        heard.push(viewer.until_finished(chat.id).await);
    }
    assert!(
        heard.iter().all(|each| *each == heard[0]),
        "every viewer heard the same: {heard:?}"
    );
    assert_eq!(heard[0].0, "Hello there");
    shut_down(&caf, running).await;
    for viewer in viewers {
        viewer.ends().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_answers_a_question_and_hears_refusals() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &[
        prompt(),
        ask_to_run(),
        json!({"await": {"to": "ask", "expect": {"result": {"outcome": {"outcome": "selected", "optionId": "allow"}}}, "timeout": 60_000}}),
        say("s1", "Ran it."),
        end_turn(),
    ])
    .await;
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    viewer.watch(chat.id).await;
    viewer
        .send(FromViewer::Send {
            request: RequestNo(1),
            chat: ChatId::from_number(999_999),
            message: UserMessage::text("Anyone?"),
        })
        .await;
    assert_eq!(viewer.done(1).await, Outcome::Refused(Refused::NoSuchChat));
    viewer
        .send(FromViewer::Send {
            request: RequestNo(2),
            chat: chat.id,
            message: UserMessage::text("Run it"),
        })
        .await;
    viewer.done(2).await;
    let question = loop {
        if let Some(ToViewer::Changes { changes, .. }) = viewer.next().await
            && let Some(question) = changes.iter().find_map(|numbered| match &numbered.change {
                Change::QuestionAsked(question) => Some(question.id),
                _ => None,
            })
        {
            break question;
        }
    };
    let answer = |request| FromViewer::Answer {
        request: RequestNo(request),
        question,
        answer: Answer::Permission(OptionId("allow".into())),
    };
    viewer.send(answer(3)).await;
    assert_eq!(viewer.done(3).await, Outcome::Done);
    viewer.send(answer(4)).await;
    assert_eq!(
        viewer.done(4).await,
        Outcome::Refused(Refused::NoSuchQuestion)
    );
    let (heard, _) = viewer.until_finished(chat.id).await;
    assert_eq!(heard, "Ran it.");
    shut_down(&caf, running).await;
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_doesnt_keep_caf_running() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &[]).await;
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    viewer.watch(chat.id).await;
    drop((caf, chat));
    assert!(
        tokio::time::timeout(PATIENCE, running).await.is_ok(),
        "CAF stops though a viewer is connected"
    );
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_while_caf_shuts_down_is_answered_gone() {
    let dir = Dir::new();
    // An adapter too busy to hear it's told to stop, and a host that waits
    // a while before stopping it firmly, so shutting down takes a while.
    let script = dir.script(
        "script.jsonl",
        &after_handshake(&[json!({"sleep": 60_000})]),
    );
    let mut host = host();
    host.timers.kill_after = std::time::Duration::from_secs(3);
    let (caf, running) = Caf::start(config_for(host, vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the session", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;
    let (mut viewer, ..) = Viewer::welcomed(&caf, opening(), 8).await;
    viewer.watch(chat.id).await;
    let shutting_down = tokio::spawn({
        let caf = caf.clone();
        async move { caf.shutdown().await }
    });
    // Shutting down has begun once the chat says so.
    loop {
        match viewer.next().await {
            Some(ToViewer::Changes { changes, .. })
                if changes.iter().any(|numbered| {
                    matches!(
                        numbered.change,
                        Change::State(ChatState::Lost {
                            reason: Lost::ShutDown
                        })
                    )
                }) =>
            {
                break;
            }
            Some(_) => {}
            None => panic!("the link ended before shutting down began"),
        }
    }
    let asks = [
        FromViewer::Open {
            request: RequestNo(1),
            open: OpenRequest::new("fake", &dir.0),
        },
        FromViewer::Send {
            request: RequestNo(2),
            chat: chat.id,
            message: UserMessage::text("Anyone?"),
        },
        FromViewer::Stop {
            request: RequestNo(3),
            chat: chat.id,
        },
        FromViewer::Answer {
            request: RequestNo(4),
            question: QuestionId(1),
            answer: Answer::Permission(OptionId("allow".into())),
        },
        FromViewer::Close {
            request: RequestNo(5),
            chat: chat.id,
        },
    ];
    for (request, ask) in (1..).zip(asks) {
        viewer.send(ask).await;
        assert_eq!(
            viewer.done(request).await,
            Outcome::Gone,
            "request {request}"
        );
    }
    drop((caf, chat, watch));
    tokio::time::timeout(PATIENCE, shutting_down)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(PATIENCE, running).await.unwrap();
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewers_that_stop_reading_dont_hold_caf_up() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &after_handshake(&[prompt()]));
    let hold = Hold::default();
    let (caf, running) =
        Caf::start_watching(config_for(host(), vec![fake(&dir, &script)]), hold.clone());
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the session", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;
    let mut viewers = Vec::new();
    for _ in 0..8 {
        let (viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 2).await;
        viewers.push(viewer);
    }
    // While CAF is held up, each viewer asks to watch thousands of ids
    // that aren't chats, and never reads what it's sent.
    hold.next_step();
    let sending = tokio::spawn({
        let chat = caf.chat(chat.id);
        async move { chat.send(UserMessage::text("Hello")).await }
    });
    hold.held().await;
    let mut sent = Vec::new();
    for viewer in &viewers {
        let to = viewer.to.clone();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        sent.push(Arc::clone(&count));
        tokio::spawn(async move {
            for id in 5000..8000 {
                let watch = FromViewer::Watch {
                    chat: ChatId::from_number(id),
                    since: None,
                };
                if to.send(watch).await.is_err() {
                    return;
                }
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
    }
    // Let CAF go once each viewer's serve has taken a couple of hundred,
    // each a watch whose read waits for CAF.
    let taken = tokio::time::timeout(PATIENCE, async {
        while (sent.iter()).any(|count| count.load(std::sync::atomic::Ordering::SeqCst) < 200) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(taken.is_ok(), "each serve takes its viewer's watches");
    hold.release();
    let patience = std::time::Duration::from_secs(10);
    let sent = tokio::time::timeout(patience, sending).await;
    assert!(matches!(sent, Ok(Ok(Ok(_)))), "the host is heard: {sent:?}");
    let stopped = tokio::time::timeout(patience, chat.stop()).await;
    assert!(stopped.is_ok(), "the host is heard after the hold, too");
    drop((caf, chat, watch));
    assert!(
        tokio::time::timeout(patience, running).await.is_ok(),
        "CAF stops though its viewers have stopped reading"
    );
    // A viewer that goes ends its serve, though it was stuck.
    for Viewer {
        to, from, served, ..
    } in viewers
    {
        drop((to, from));
        assert!(matches!(
            tokio::time::timeout(PATIENCE, served).await,
            Ok(Ok(()))
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_more_comes_for_an_unwatched_chat() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &hello_there()).await;
    // Open, so nothing is on its way for the chat when it's unwatched.
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the session", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    viewer.watch(chat.id).await;
    viewer.send(FromViewer::Unwatch { chat: chat.id }).await;
    // Answered after the unwatch, so it has been done.
    viewer
        .send(FromViewer::Stop {
            request: RequestNo(1),
            chat: ChatId::from_number(999_999),
        })
        .await;
    viewer.done(1).await;
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Say hello")).await.unwrap();
    until(&chat, &mut watch, "the turn", finished).await;
    viewer
        .send(FromViewer::Stop {
            request: RequestNo(2),
            chat: ChatId::from_number(999_999),
        })
        .await;
    viewer.done(2).await;
    assert!(
        viewer.skipped.is_empty(),
        "nothing came for the chat: {:?}",
        viewer.skipped
    );
    shut_down(&caf, running).await;
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_watches_only_as_many_chats_as_the_host_allows() {
    let dir = Dir::new();
    // Both chats' sessions, from one adapter.
    let script = dir.script(
        "script.jsonl",
        &after_handshake(&[
            json!({"wait": {"method": "session/new"}}),
            json!({"respond": {"result": {"sessionId": "s2"}}}),
        ]),
    );
    let (caf, running) = Caf::start(config_for(host(), vec![fake(&dir, &script)]));
    let first = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let second = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    // Open, so nothing changes in them meanwhile.
    for chat in [&first, &second] {
        let mut watch = chat.watch();
        until(chat, &mut watch, "the session", |chat| {
            matches!(chat.state, ChatState::Open { .. })
        })
        .await;
    }
    let mut rules = ViewerRules::default();
    rules.max_watches = 1;
    let (mut viewer, ..) = Viewer::welcomed(&caf, rules, 8).await;
    viewer.watch(first.id).await;
    viewer
        .send(FromViewer::Watch {
            chat: second.id,
            since: None,
        })
        .await;
    assert_eq!(
        viewer.next().await,
        Some(ToViewer::Ended { chat: second.id })
    );
    // Watching the same chat again isn't one more.
    viewer.watch(first.id).await;
    // And an unwatched one makes room.
    viewer.send(FromViewer::Unwatch { chat: first.id }).await;
    viewer.watch(second.id).await;
    shut_down(&caf, running).await;
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watching_a_chat_again_before_anything_comes_starts_it_once() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &after_handshake(&hello_there()));
    let hold = Hold::default();
    let (caf, running) =
        Caf::start_watching(config_for(host(), vec![fake(&dir, &script)]), hold.clone());
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    let (_, before) = viewer.watch(chat.id).await;
    viewer.send(FromViewer::Unwatch { chat: chat.id }).await;
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Say hello")).await.unwrap();
    until(&chat, &mut watch, "the turn", finished).await;
    // While CAF is held up, so nothing can come for the chat, the viewer
    // asks for it over and over.
    let held = || {
        let caf = caf.clone();
        let hold = hold.clone();
        let folder = dir.0.clone();
        async move {
            hold.next_step();
            // Something the core takes, so CAF takes a step.
            tokio::spawn(async move { caf.open_chat(OpenChat::new("fake", folder)).await });
            hold.held().await;
        }
    };
    let asks = |since| FromViewer::Watch {
        chat: chat.id,
        since,
    };
    held().await;
    for _ in 0..5 {
        viewer.send(asks(None)).await;
    }
    viewer.taken().await;
    hold.release();
    loop {
        match viewer.next().await {
            Some(ToViewer::Picture { chat: whole, .. }) if whole.id == chat.id => break,
            Some(ToViewer::Chats(_) | ToViewer::Changes { .. }) => {}
            other => panic!("the chat's picture, not {other:?}"),
        }
    }
    viewer
        .send(FromViewer::Stop {
            request: RequestNo(1),
            chat: ChatId::from_number(999_999),
        })
        .await;
    viewer.done(1).await;
    let again =
        (viewer.skipped.drain(..)).find(|message| matches!(message, ToViewer::Picture { .. }));
    assert_eq!(again, None, "one picture, however often it was asked for");

    // Asked from a change it's missed things since, then from nothing: it
    // gets the picture asked for last, and not the changes.
    viewer.send(FromViewer::Unwatch { chat: chat.id }).await;
    held().await;
    viewer.send(asks(Some(before))).await;
    viewer.send(asks(None)).await;
    viewer.taken().await;
    hold.release();
    loop {
        match viewer.next().await {
            Some(ToViewer::Chats(_)) => {}
            Some(ToViewer::Picture { chat: whole, .. }) if whole.id == chat.id => break,
            other => panic!("the picture asked for last, not {other:?}"),
        }
    }
    shut_down(&caf, running).await;
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_ends_though_its_viewer_has_stopped_reading() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &[]).await;
    // Each has room for one message, filled with an answer it never reads.
    let filled = async || {
        let (viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 1).await;
        viewer
            .send(FromViewer::Stop {
                request: RequestNo(1),
                chat: ChatId::from_number(999_999),
            })
            .await;
        viewer
    };
    // One stops sending too.
    let Viewer {
        to, from, served, ..
    } = filled().await;
    drop(to);
    assert!(
        matches!(tokio::time::timeout(PATIENCE, served).await, Ok(Ok(()))),
        "serve ends when its viewer stops sending"
    );
    drop(from);
    // One goes on as it is, watching nothing, until CAF shuts down.
    let Viewer {
        to, from, served, ..
    } = filled().await;
    drop(chat);
    shut_down(&caf, running).await;
    assert!(
        matches!(tokio::time::timeout(PATIENCE, served).await, Ok(Ok(()))),
        "serve ends with CAF"
    );
    drop((to, from));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_viewer_hears_its_cafs_run_and_each_caf_has_its_own() {
    let (one, one_running) = Caf::start(config_for(host(), Vec::new()));
    let (other, other_running) =
        Caf::start_watching(config_for(host(), Vec::new()), Hold::default());
    assert_ne!(one.run(), other.run());
    for caf in [&one, &other] {
        let run = caf.run();
        assert_eq!(run.len(), 32, "128 bits as hex: {run}");
        assert!(run.bytes().all(|byte| byte.is_ascii_hexdigit()), "{run}");
    }
    let mut viewers = Vec::new();
    for _ in 0..3 {
        // Each checks it hears `one`'s run.
        let (viewer, ..) = Viewer::welcomed(&one, ViewerRules::default(), 8).await;
        viewers.push(viewer);
    }
    let (watching, ..) = Viewer::welcomed(&other, ViewerRules::default(), 8).await;
    shut_down(&one, one_running).await;
    shut_down(&other, other_running).await;
    for viewer in viewers {
        viewer.ends().await;
    }
    watching.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asking_for_an_idle_chat_whole_gets_it_whatever_was_asked_before() {
    let dir = Dir::new();
    let (caf, running, chat) = started(&dir, &[]).await;
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the session", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    let (_, latest) = viewer.watch(chat.id).await;
    // Resumed from where it is, as after reconnecting: there's nothing
    // to send while the chat is idle. Then asked for whole.
    viewer.send(FromViewer::Unwatch { chat: chat.id }).await;
    viewer
        .send(FromViewer::Watch {
            chat: chat.id,
            since: Some(latest),
        })
        .await;
    viewer
        .send(FromViewer::Watch {
            chat: chat.id,
            since: None,
        })
        .await;
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), viewer.next()).await;
    match got {
        Ok(Some(ToViewer::Picture { chat: whole, .. })) => assert_eq!(whole.id, chat.id),
        other => panic!("the chat whole, not {other:?}"),
    }
    shut_down(&caf, running).await;
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_chooses_a_setting_and_learns_an_agents_as_the_host_allows() {
    let dir = Dir::new();
    let mut script = handshake_offering("s1");
    script.extend(set_to(
        "model",
        json!("large"),
        &options("large", "low", false),
    ));
    // Learning, on the chat's adapter, in a session of its own.
    script.extend([
        json!({"wait": {"method": "session/new", "expect": {"params": {"cwd": "/"}}}}),
        json!({"respond": {"result": {"sessionId": "learn", "configOptions": options("small", "high", true)}}}),
    ]);
    let script = dir.script("script.jsonl", &script);
    let (caf, running) = Caf::start(config_for(host(), vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the session", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;

    // Choosing is acting on a chat: allowed by default. Learning is
    // acting on an agent: not.
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    viewer
        .send(FromViewer::Set {
            request: RequestNo(1),
            chat: chat.id,
            choice: SettingChoice::model("large"),
        })
        .await;
    assert_eq!(viewer.done(1).await, Outcome::Done);
    viewer
        .send(FromViewer::Set {
            request: RequestNo(2),
            chat: chat.id,
            choice: SettingChoice::model("huge"),
        })
        .await;
    assert_eq!(
        viewer.done(2).await,
        Outcome::Refused(Refused::NoSuchSetting),
        "the core's own refusal"
    );
    until(&chat, &mut watch, "the model to change", |chat| {
        (chat.settings.offered.iter()).any(|setting| {
            setting.kind == SettingKind::Model
                && setting.current == Some(SettingValue::Choice("large".into()))
        })
    })
    .await;
    let fake = AgentName("fake".into());
    viewer
        .send(FromViewer::Agent {
            request: RequestNo(3),
            agent: fake.clone(),
            action: AgentAction::LearnSettings,
        })
        .await;
    assert_eq!(viewer.done(3).await, Outcome::NotAllowed);

    let mut rules = ViewerRules::default();
    rules.agents = true;
    let (mut learner, ..) = Viewer::welcomed(&caf, rules, 8).await;
    learner
        .send(FromViewer::Agent {
            request: RequestNo(1),
            agent: fake,
            action: AgentAction::LearnSettings,
        })
        .await;
    assert_eq!(learner.done(1).await, Outcome::Done);
    // What the learning session offered, as it said it is there.
    let learned = tokio::time::timeout(PATIENCE, async {
        loop {
            let statuses = match learner.statuses.pop_front() {
                Some(statuses) => statuses,
                None => match learner.received().await {
                    Some(ToViewer::Agents(statuses)) => statuses,
                    Some(_) => continue,
                    None => panic!("the link ended"),
                },
            };
            if let KnownSettings::Known(known) = &statuses[0].settings
                && known
                    .iter()
                    .any(|setting| setting.current == Some(SettingValue::Choice("high".into())))
            {
                return known.clone();
            }
        }
    })
    .await
    .expect("learned in good time");
    assert_eq!(learned.len(), 3);
    let now = picture(&chat).await;
    assert_eq!(
        now.settings
            .offered
            .iter()
            .find(|setting| setting.kind == SettingKind::Effort)
            .and_then(|setting| setting.current.clone()),
        Some(SettingValue::Choice("low".into())),
        "the chat's own settings are its session's"
    );
    shut_down(&caf, running).await;
    viewer.ends().await;
    learner.ends().await;
    assert!(
        !dir.record().iter().any(|line| line.get("failed").is_some()),
        "{:#?}",
        dir.record()
    );
}

/// Rules that let viewers read usage and set prices.
fn usage_rules() -> ViewerRules {
    let mut rules = ViewerRules::default();
    rules.usage = true;
    rules
}

/// January 2026, by day in London.
fn january() -> UsageQuery {
    UsageQuery::new(
        Wall(1_767_225_600_000),
        Wall(1_769_904_000_000),
        "Europe/London",
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_reads_usage_and_sets_prices_only_if_the_host_lets_it() {
    let dir = Dir::new();
    let (caf, running) = Caf::start(config_with_ledger(&dir, host(), Vec::new()));
    let model = "claude-viewer-1";
    let (mut viewer, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 64).await;
    viewer
        .send(FromViewer::Usage {
            request: RequestNo(1),
            query: january(),
        })
        .await;
    assert_eq!(viewer.done(1).await, Outcome::NotAllowed);
    let price = Some(ModelPrice::new(1.0, 2.0));
    viewer
        .send(FromViewer::SetPrice {
            request: RequestNo(2),
            model: model.into(),
            price: price.clone(),
        })
        .await;
    assert_eq!(viewer.done(2).await, Outcome::NotAllowed);
    assert!(caf.prices().await.unwrap().find(model).is_none(), "not set");

    let (mut viewer, ..) = Viewer::welcomed(&caf, usage_rules(), 64).await;
    viewer
        .send(FromViewer::SetPrice {
            request: RequestNo(3),
            model: model.into(),
            price,
        })
        .await;
    assert_eq!(viewer.done(3).await, Outcome::Done);
    let prices = caf.prices().await.unwrap();
    assert_eq!(
        prices.find(model).map(|own| own.layer),
        Some(PriceLayer::Own)
    );
    viewer
        .send(FromViewer::Usage {
            request: RequestNo(4),
            query: january(),
        })
        .await;
    match viewer.next().await {
        Some(ToViewer::Usage {
            request: RequestNo(4),
            summary,
        }) => {
            assert_eq!(summary.query, january());
            assert!(summary.buckets.is_empty(), "nothing was spent");
        }
        other => panic!("the summary, not {other:?}"),
    }
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_hears_there_is_no_ledger_or_that_a_query_is_bad() {
    let dir = Dir::new();
    let (caf, running) = Caf::start(config_for(host(), Vec::new()));
    let (mut viewer, ..) = Viewer::welcomed(&caf, usage_rules(), 64).await;
    viewer
        .send(FromViewer::Usage {
            request: RequestNo(1),
            query: january(),
        })
        .await;
    assert_eq!(viewer.done(1).await, Outcome::NoLedger);
    viewer
        .send(FromViewer::SetPrice {
            request: RequestNo(2),
            model: "claude-viewer-1".into(),
            price: None,
        })
        .await;
    assert_eq!(viewer.done(2).await, Outcome::NoLedger);
    shut_down(&caf, running).await;

    let (caf, running) = Caf::start(config_with_ledger(&dir, host(), Vec::new()));
    let (mut viewer, ..) = Viewer::welcomed(&caf, usage_rules(), 64).await;
    let mut query = january();
    query.zone = "Nowhere/Atlantis".into();
    viewer
        .send(FromViewer::Usage {
            request: RequestNo(1),
            query,
        })
        .await;
    assert_eq!(
        viewer.done(1).await,
        Outcome::BadQuery(BadQuery::Zone("Nowhere/Atlantis".into()))
    );
    let mut query = january();
    query.until = query.since;
    viewer
        .send(FromViewer::Usage {
            request: RequestNo(2),
            query,
        })
        .await;
    assert_eq!(viewer.done(2).await, Outcome::BadQuery(BadQuery::Period));
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_summary_never_holds_up_a_viewers_other_requests() {
    let dir = Dir::new();
    // January's file, as a pipe: reading it waits until something writes
    // to it.
    let app = dir.path("usage/blocked");
    std::fs::create_dir_all(&app).unwrap();
    let pipe = app.join("2026-01.jsonl");
    let path = std::ffi::CString::new(pipe.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a plain syscall, with a path that lives through it.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let (caf, running) = Caf::start(config_with_ledger(&dir, host(), Vec::new()));
    let (mut viewer, ..) = Viewer::welcomed(&caf, usage_rules(), 64).await;
    viewer
        .send(FromViewer::Usage {
            request: RequestNo(1),
            query: january(),
        })
        .await;
    viewer
        .send(FromViewer::Open {
            request: RequestNo(2),
            open: OpenRequest::new("fake", &dir.0),
        })
        .await;
    assert_eq!(
        viewer.done(2).await,
        Outcome::NotAllowed,
        "answered at once"
    );
    viewer
        .send(FromViewer::SetPrice {
            request: RequestNo(3),
            model: "claude-viewer-1".into(),
            price: None,
        })
        .await;
    assert_eq!(viewer.done(3).await, Outcome::Done, "so is a price");
    assert!(
        !(viewer.skipped.iter()).any(|message| matches!(message, ToViewer::Usage { .. })),
        "the summary waits for the pipe"
    );
    // Something opens it to write, once the summary is reading it, and
    // writes nothing: then the summary is read.
    let opened = tokio::time::timeout(PATIENCE, async {
        loop {
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&pipe);
            match writer {
                Ok(writer) => return drop(writer),
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
    });
    opened.await.expect("the summary reads the pipe");
    match viewer.next().await {
        Some(ToViewer::Usage {
            request: RequestNo(1),
            ..
        }) => {}
        other => panic!("the summary, not {other:?}"),
    }
    shut_down(&caf, running).await;
}
