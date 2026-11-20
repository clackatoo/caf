//! Serving viewers, over in-memory channels, against the fake adapter.

mod common;

use std::sync::Arc;

use caf_core::change::{Change, Numbered};
use caf_core::chat::{ChatState, ItemKind, Lost, TurnLife, UserMessage};
use caf_core::ids::{AgentName, ChangeNo, ChatId, QuestionId};
use caf_core::question::{Answer, OptionId};
use caf_core::viewer::{ChatSummary, FromViewer, OpenRequest, Outcome, RequestNo, ToViewer};
use caf_core::{Chat, OpenChat, Refused};
use caf_driver_tokio::viewer::{ViewerRules, serve};
use caf_driver_tokio::{Caf, Config, Running};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use common::*;

/// One viewer, at the other end of `serve`.
struct Viewer {
    to: mpsc::Sender<FromViewer>,
    from: mpsc::Receiver<ToViewer>,
    served: JoinHandle<()>,
}

impl Viewer {
    /// Connects a viewer, with room for `room` messages to it.
    fn connect(caf: &Caf, rules: ViewerRules, room: usize) -> Self {
        let (to, from_viewer) = mpsc::channel(16);
        let (to_viewer, from) = mpsc::channel(room);
        let served = tokio::spawn(serve(caf.clone(), Arc::new(rules), from_viewer, to_viewer));
        Self { to, from, served }
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
            }) => (viewer, agents, chats),
            other => panic!("a welcome, not {other:?}"),
        }
    }

    async fn send(&self, message: FromViewer) {
        self.to.send(message).await.expect("serve is still running");
    }

    /// The next message, or `None` once the link has ended.
    async fn next(&mut self) -> Option<ToViewer> {
        tokio::time::timeout(PATIENCE, self.from.recv())
            .await
            .expect("a message, or the link's end, in good time")
    }

    /// Reads until the answer to `request`, giving it and what came
    /// before it.
    async fn done(&mut self, request: u64) -> (Outcome, Vec<ToViewer>) {
        let mut before = Vec::new();
        loop {
            match self.next().await {
                Some(ToViewer::Done {
                    request: answered,
                    outcome,
                }) if answered == RequestNo(request) => return (outcome, before),
                Some(other) => before.push(other),
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
    let (caf, running) = Caf::start(Config::new(host(), vec![fake(dir, &script)]));
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
    }) = viewer.next().await
    else {
        panic!("a welcome")
    };
    assert_eq!(version, 1);
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
    let (outcome, _) = viewer.done(1).await;
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
    let (caf, running) = Caf::start(Config::new(host, vec![fake(&dir, &script)]));
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
    let (caf, running) = Caf::start(Config::new(host, vec![fake(&dir, &script)]));
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
    assert_eq!(viewer.done(1).await, (Outcome::NotAllowed, vec![]));

    let (mut allowed, ..) = Viewer::welcomed(&caf, opening(), 8).await;
    allowed
        .send(FromViewer::Open {
            request: RequestNo(2),
            open: OpenRequest::new("fake", &dir.0),
        })
        .await;
    let (outcome, _) = allowed.done(2).await;
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
        allowed.done(3).await.0,
        Outcome::Refused(Refused::RelativeFolder)
    );
    allowed
        .send(FromViewer::Open {
            request: RequestNo(4),
            open: OpenRequest::new("nobody", &dir.0),
        })
        .await;
    assert_eq!(
        allowed.done(4).await.0,
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
    assert_eq!(buggy.done(5).await.0, Outcome::NotAllowed);
    buggy
        .send(FromViewer::Stop {
            request: RequestNo(6),
            chat: ChatId(999_999),
        })
        .await;
    assert_eq!(buggy.done(6).await.0, Outcome::Refused(Refused::NoSuchChat));
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
    ];
    for (request, ask) in (1..).zip(asks) {
        viewer.send(ask).await;
        assert_eq!(viewer.done(request).await.0, Outcome::NotAllowed);
    }
    let now = picture(&chat).await;
    assert!(now.turns.is_empty());
    assert_ne!(now.state, ChatState::Closed);
    // Opening is the host's rule's to decide, not `act`'s.
    viewer
        .send(FromViewer::Open {
            request: RequestNo(5),
            open: OpenRequest::new("fake", &dir.0),
        })
        .await;
    assert!(matches!(viewer.done(5).await.0, Outcome::Opened(_)));
    shut_down(&caf, running).await;
    viewer.ends().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_hears_of_chats_others_open_and_close() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &handshake("s1"));
    let (caf, running) = Caf::start(Config::new(host(), vec![fake(&dir, &script)]));
    let (mut opener, ..) = Viewer::welcomed(&caf, opening(), 8).await;
    let (mut other, _, chats) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    assert!(chats.is_empty());
    opener
        .send(FromViewer::Open {
            request: RequestNo(1),
            open: OpenRequest::new("fake", &dir.0),
        })
        .await;
    let Outcome::Opened(first) = opener.done(1).await.0 else {
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
    assert_eq!(opener.done(2).await.0, Outcome::Done);
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
    let (outcome, mut heard) = viewer.done(9).await;
    assert_eq!(outcome, Outcome::Done);
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
    let never = ChatId(999_999);
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
    let Viewer { to, from, served } = viewer;
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
    let Viewer { to, from, served } = quiet;
    drop(to);
    assert!(matches!(
        tokio::time::timeout(PATIENCE, served).await,
        Ok(Ok(()))
    ));
    drop(from);
    // The viewer stops listening, while nothing changes.
    let (deaf, ..) = Viewer::welcomed(&caf, ViewerRules::default(), 8).await;
    let Viewer { to, from, served } = deaf;
    drop(from);
    assert!(matches!(
        tokio::time::timeout(PATIENCE, served).await,
        Ok(Ok(()))
    ));
    drop(to);
    // Before the handshake, too.
    let silent = Viewer::connect(&caf, ViewerRules::default(), 8);
    let Viewer { to, from, served } = silent;
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
            chat: ChatId(999_999),
            message: UserMessage::text("Anyone?"),
        })
        .await;
    assert_eq!(
        viewer.done(1).await.0,
        Outcome::Refused(Refused::NoSuchChat)
    );
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
    assert_eq!(viewer.done(3).await.0, Outcome::Done);
    viewer.send(answer(4)).await;
    assert_eq!(
        viewer.done(4).await.0,
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
