//! Live, with the fake agent: a chat through `/viewer`, its steps on
//! `/inspect`, the replay check, and the recording.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use caf_core::Chat;
use caf_core::agent::{AgentSpec, AgentStatus, SpawnCommand};
use caf_core::limits::{AccountLimits, LimitState, WindowKind};
use caf_core::profiles::{Builtin, ProfileChoice};
use caf_core::viewer::{ChatCopy, ToViewer};
use caf_debugger::inspector::{FromInspector, Mode, ToInspector};
use caf_debugger::live::Live;
use caf_debugger::replay::{self, BUDGET, Player};
use caf_debugger::server::{self, App};
use caf_debugger::{fake, live};
use caf_driver_tokio::UsageConfig;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// How long anything here may take.
const PATIENCE: Duration = Duration::from_secs(20);

/// A folder of its own, removed when dropped.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("caf-debugger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A usage ledger in `dir`, which never fetches prices: never the user's.
fn ledger(dir: &Dir) -> Option<UsageConfig> {
    let mut usage = UsageConfig::new(dir.0.join("usage"));
    usage.fetch_prices = false;
    Some(usage)
}

/// Opens a WebSocket as the app does.
async fn open(port: u16, path: &str, token: &str) -> Socket {
    let url = format!("ws://127.0.0.1:{port}{path}?token={token}");
    let mut request = url.into_client_request().unwrap();
    let origin = HeaderValue::from_str(&format!("http://127.0.0.1:{port}")).unwrap();
    request.headers_mut().insert("Origin", origin);
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("it opens");
    socket
}

/// The next text message, as JSON, or `None` once the server has closed
/// the socket, with its close code.
async fn next(socket: &mut Socket) -> Result<Value, Option<u16>> {
    loop {
        let message = tokio::time::timeout(PATIENCE, socket.next())
            .await
            .expect("a message comes");
        match message {
            Some(Ok(Message::Text(text))) => return Ok(serde_json::from_str(&text).unwrap()),
            Some(Ok(Message::Close(frame))) => return Err(frame.map(|frame| frame.code.into())),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return Err(None),
        }
    }
}

async fn send(socket: &mut Socket, message: Value) {
    socket
        .send(Message::text(message.to_string()))
        .await
        .unwrap();
}

/// The viewer's messages until one `until` accepts, which is returned.
async fn viewer_until(socket: &mut Socket, until: impl Fn(&Value) -> bool) -> Value {
    loop {
        let message = next(socket).await.expect("the viewer is open");
        if until(&message) {
            return message;
        }
    }
}

/// [`viewer_until`], keeping in `agents` the agents' statuses last told
/// on the way.
async fn viewer_until_noting(
    socket: &mut Socket,
    agents: &mut Vec<AgentStatus>,
    until: impl Fn(&Value) -> bool,
) -> Value {
    loop {
        let message = next(socket).await.expect("the viewer is open");
        if message["type"] == "Agents" {
            *agents = serde_json::from_value(message["value"].clone()).unwrap();
        }
        if until(&message) {
            return message;
        }
    }
}

/// The inspector's steps, collected as they come, and anything else.
#[derive(Default)]
struct Seen {
    steps: Vec<Value>,
    others: Vec<ToInspector>,
}

impl Seen {
    /// Reads what's come, until a message `until` accepts.
    async fn until(&mut self, socket: &mut Socket, until: impl Fn(&ToInspector) -> bool) {
        loop {
            let text = next(socket)
                .await
                .expect("the inspector is open")
                .to_string();
            let message: ToInspector = serde_json::from_str(&text).unwrap();
            if let ToInspector::Steps { from, steps } = &message {
                assert_eq!(*from, self.steps.len() as u64 + 1, "steps with no gaps");
                for step in steps {
                    self.steps.push(serde_json::to_value(step).unwrap());
                }
            }
            let done = until(&message);
            self.others.push(message);
            if done {
                return;
            }
        }
    }

    /// Reads what's come until it has steps up to `end`.
    async fn steps_to(&mut self, socket: &mut Socket, end: u64) {
        while (self.steps.len() as u64) < end {
            self.until(socket, |message| {
                matches!(message, ToInspector::Steps { .. })
            })
            .await;
        }
    }

    fn diverged(&self) -> bool {
        (self.others.iter()).any(|message| matches!(message, ToInspector::Diverged { .. }))
    }
}

/// A plain GET of `target`: its status and body.
async fn get(port: u16, target: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let request =
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split = (response.windows(4))
        .position(|window| window == b"\r\n\r\n")
        .expect("a whole response");
    let head = String::from_utf8(response[..split].to_vec()).unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, response[split + 4..].to_vec())
}

/// The lives of a chat's turns, in order.
fn lives(chat: &Chat) -> Vec<String> {
    (chat.turns.iter())
        .map(|turn| {
            serde_json::to_value(&turn.life).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// The debugger, live with the fake agent, served on a port of its own.
struct Debugger {
    dir: Dir,
    live: Live,
    port: u16,
    token: String,
    recording: PathBuf,
}

impl Debugger {
    async fn start(name: &str) -> Self {
        Self::start_with(name, None).await
    }

    /// The debugger, keeping chats in `store` if given.
    async fn start_with(name: &str, store: Option<PathBuf>) -> Self {
        Self::start_fake(name, store, None).await
    }

    /// The debugger, its fake saying its account's limits, made up from
    /// `limits`, if given, as `--fake-limits` asks.
    async fn start_fake(name: &str, store: Option<PathBuf>, limits: Option<SystemTime>) -> Self {
        let dir = Dir::new(name);
        let script = fake::write(&dir.0, false, limits).unwrap();
        let mut command = SpawnCommand::new(caf_fake_agent::binary());
        command.args = vec![script.into_os_string()];
        let agents = vec![AgentSpec::command(
            "fake",
            command,
            ProfileChoice::Builtin(Builtin::Fake),
        )];
        let recording = dir.0.join("run.jsonl");
        let live = live::start(agents, Some(recording.clone()), store, ledger(&dir));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = server::new_token();
        let mut app = App::new(Arc::clone(&live.book), port, token.clone());
        app.live = Some((live.caf.clone(), Arc::clone(&live.rules)));
        app.recording = Some(recording.clone());
        let router = server::router(Arc::new(app));
        tokio::spawn(async move { axum::serve(listener, router).await });
        Self {
            dir,
            live,
            port,
            token,
            recording,
        }
    }

    /// Shuts CAF down, and checks every step it took never diverged from
    /// either replay.
    async fn stop(self) {
        self.live.caf.shutdown().await;
        let Debugger {
            live, dir: _dir, ..
        } = self;
        live.running.await;
        live.book.all_in().await;
        assert!(
            live.book.diverged().is_empty(),
            "{:?}",
            live.book.diverged()
        );
        assert_eq!(live.book.recording_checked(), live.book.end());
    }

    /// The viewer wire, with a chat opened and watched.
    async fn chat(&self) -> Viewer {
        let mut socket = open(self.port, "/viewer", &self.token).await;
        send(
            &mut socket,
            json!({"type": "Hello", "value": {"min": 1, "max": 1}}),
        )
        .await;
        let welcome = next(&mut socket).await.unwrap();
        assert_eq!(welcome["type"], "Welcome");
        assert_eq!(welcome["value"]["run"], self.live.caf.run());
        assert_eq!(welcome["value"]["agents"], json!(["fake"]));
        let cwd = self.dir.0.display().to_string();
        send(&mut socket, json!({"type": "Open", "value": {"request": 1, "open": {"agent": "fake", "cwd": cwd, "session": {"type": "New"}}}})).await;
        let mut agents = Vec::new();
        let done = viewer_until_noting(&mut socket, &mut agents, |message| {
            message["type"] == "Done"
        })
        .await;
        assert_eq!(done["value"]["outcome"]["type"], "Opened", "{done}");
        let chat = done["value"]["outcome"]["value"].clone();
        send(
            &mut socket,
            json!({"type": "Watch", "value": {"chat": chat, "since": null}}),
        )
        .await;
        let picture = viewer_until_noting(&mut socket, &mut agents, |message| {
            message["type"] == "Picture"
        })
        .await;
        let ToViewer::Picture {
            chat: whole,
            latest,
        } = serde_json::from_value(picture).unwrap()
        else {
            unreachable!()
        };
        Viewer {
            socket,
            chat,
            copy: ChatCopy::new(whole, latest),
            requests: 1,
            agents,
        }
    }
}

/// A viewer's link, following one chat.
struct Viewer {
    socket: Socket,
    chat: Value,
    copy: ChatCopy,
    requests: u64,
    /// The agents' statuses, as last told.
    agents: Vec<AgentStatus>,
}

impl Viewer {
    /// Sends a request about the chat: `fields` with its number.
    async fn request(&mut self, kind: &str, mut fields: Value) {
        self.requests += 1;
        fields["request"] = json!(self.requests);
        send(&mut self.socket, json!({"type": kind, "value": fields})).await;
    }

    async fn say(&mut self, text: &str) {
        let chat = self.chat.clone();
        let message = json!({"parts": [{"type": "Text", "value": text}]});
        self.request("Send", json!({"chat": chat, "message": message}))
            .await;
    }

    /// Follows the chat until `until` holds of it.
    async fn until(&mut self, until: impl Fn(&Chat) -> bool) {
        let started = Instant::now();
        while !until(self.copy.chat()) {
            assert!(started.elapsed() < PATIENCE, "{:#?}", self.copy.chat());
            let message = next(&mut self.socket).await.expect("the viewer is open");
            match serde_json::from_value(message).unwrap() {
                ToViewer::Picture { chat, latest } => self.copy = ChatCopy::new(chat, latest),
                ToViewer::Changes { changes, .. } => {
                    self.copy.take(&changes).expect("the changes follow on");
                }
                ToViewer::Agents(agents) => self.agents = agents,
                _ => {}
            }
        }
    }

    /// Follows the chat until `until` holds of the fake's limits.
    async fn until_limits(&mut self, until: impl Fn(&AccountLimits) -> bool) {
        let started = Instant::now();
        let fakes = |agents: &[AgentStatus]| {
            (agents.iter())
                .find(|agent| agent.name.0 == "fake")
                .map(|agent| agent.limits.clone())
                .unwrap_or_default()
        };
        while !until(&fakes(&self.agents)) {
            assert!(started.elapsed() < PATIENCE, "{:#?}", fakes(&self.agents));
            let message = next(&mut self.socket).await.expect("the viewer is open");
            match serde_json::from_value(message).unwrap() {
                ToViewer::Picture { chat, latest } => self.copy = ChatCopy::new(chat, latest),
                ToViewer::Changes { changes, .. } => {
                    self.copy.take(&changes).expect("the changes follow on");
                }
                ToViewer::Agents(agents) => self.agents = agents,
                _ => {}
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_with_the_fake_agent_the_steps_arrive_and_never_diverge() {
    let debugger = Debugger::start("live").await;
    let (port, token, recording) = (
        debugger.port,
        debugger.token.clone(),
        debugger.recording.clone(),
    );
    let running = &debugger.live;

    // The inspector, from the start.
    let mut inspector = open(port, "/inspect", &token).await;
    let mut seen = Seen::default();
    seen.until(&mut inspector, |message| {
        matches!(message, ToInspector::Position { .. })
    })
    .await;
    let ToInspector::Hello {
        mode, run, file, ..
    } = &seen.others[0]
    else {
        panic!("Hello first");
    };
    assert_eq!(*mode, Mode::Live);
    assert_eq!(run.as_deref(), Some(running.caf.run()));
    assert_eq!(
        file.as_deref(),
        Some(recording.display().to_string().as_str())
    );
    // Following live, at the end, having been sent every step up to it.
    // How many that is depends on how far CAF had got: its first steps,
    // looking at its agents, may come before or after.
    assert!(
        matches!(
            seen.others.last(),
            Some(ToInspector::Position { live: true, at, end })
                if at == end && *end == seen.steps.len() as u64
        ),
        "{:?} after {} steps",
        seen.others.last(),
        seen.steps.len()
    );

    // A chat, through the viewer wire.
    let mut viewer = debugger.chat().await;
    let chat = viewer.chat.clone();
    viewer.say("hello").await;
    viewer.until(|chat| lives(chat) == ["Finished"]).await;

    // `ask`: a permission question, answered.
    viewer.say(fake::ASK).await;
    viewer.until(|chat| !chat.questions.is_empty()).await;
    let question = serde_json::to_value(&viewer.copy.chat().questions[0]).unwrap();
    let allow = (question["kind"]["value"]["options"]
        .as_array()
        .unwrap()
        .iter())
    .find(|option| option["kind"]["type"] == "AllowOnce")
    .map(|option| option["id"].clone())
    .unwrap_or_else(|| panic!("an option to allow: {question:#}"));
    let answer =
        json!({"question": question["id"], "answer": {"type": "Permission", "value": allow}});
    viewer.request("Answer", answer).await;
    viewer
        .until(|chat| lives(chat) == ["Finished", "Finished"])
        .await;
    let mut viewer = viewer.socket;

    // The inspector saw every step, and the replay check never failed.
    let end = running.book.end();
    seen.steps_to(&mut inspector, end).await;
    assert!(end > 10, "{end} steps");
    assert_eq!(
        seen.steps[0]["event"],
        json!({"type": "Host", "value": {"type": "CheckAgents"}}),
        "CAF looks at its agents first"
    );
    assert!(running.book.diverged().is_empty());
    assert!(!seen.diverged());
    let writes = (seen.steps.iter())
        .flat_map(|step| step["directives"].as_array().unwrap().clone())
        .filter(|directive| directive["value"]["type"] == "Write")
        .count();
    assert!(
        writes > 3,
        "the steps carry the directives: {writes} writes"
    );

    // Looking back to just after the chat was opened, then following
    // again. What came before it, such as the prices, may come in any
    // order.
    let opened = (seen.steps.iter())
        .position(|step| step["event"]["value"]["type"] == "OpenChat")
        .expect("the chat was opened") as u64
        + 1;
    send(
        &mut inspector,
        json!({"type": "Seek", "value": {"step": opened}}),
    )
    .await;
    seen.until(&mut inspector, |message| {
        matches!(message, ToInspector::Position { .. })
    })
    .await;
    let Some(ToInspector::Position {
        live: false,
        at,
        end: now,
    }) = seen.others.last()
    else {
        panic!("looking back at {opened}: {:?}", seen.others.last());
    };
    assert_eq!(*at, opened);
    assert!(*now >= end);
    seen.until(&mut inspector, |message| matches!(message, ToInspector::Past { message, .. } if serde_json::to_value(message).unwrap()["type"] == "Picture")).await;
    let follow = serde_json::to_value(FromInspector::Follow).unwrap();
    send(&mut inspector, follow).await;
    seen.until(&mut inspector, |message| {
        matches!(message, ToInspector::Position { .. })
    })
    .await;
    assert!(matches!(
        seen.others.last(),
        Some(ToInspector::Position { live: true, .. })
    ));

    // Closed, the chat is archived, and still watched, as it may be
    // unarchived; and it's still in what looking back shows.
    send(
        &mut viewer,
        json!({"type": "Close", "value": {"request": 99, "chat": chat}}),
    )
    .await;
    let archived = viewer_until(&mut viewer, |message| {
        assert_ne!(message["type"], "Ended", "the watch goes on");
        message["type"] == "Changes"
            && message["value"]["chat"] == chat
            && message.to_string().contains(r#""Archived""#)
    })
    .await;
    assert_eq!(archived["value"]["chat"], chat);
    // The close's step may still be on its way into the book.
    let started = Instant::now();
    while !(running.book.after(0).iter()).any(|step| step.event.get().contains(r#""CloseChat""#)) {
        assert!(started.elapsed() < PATIENCE, "the close's step comes");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    send(
        &mut inspector,
        json!({"type": "Seek", "value": {"step": u64::MAX}}),
    )
    .await;
    let pasts = seen.others.len();
    seen.until(&mut inspector, |message| matches!(message, ToInspector::Past { message, .. } if serde_json::to_value(message).unwrap()["type"] != "Chats")).await;
    let past: Vec<Value> = (seen.others[pasts..].iter())
        .filter_map(|message| match message {
            ToInspector::Past { message, .. } => Some(serde_json::to_value(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(past[0]["type"], "Chats");
    assert_eq!(
        past[0]["value"][0]["id"], chat,
        "a closed chat is listed: {past:?}"
    );
    assert_eq!(
        past[1]["type"], "Picture",
        "first after following: {past:?}"
    );
    assert_eq!(past[1]["value"]["chat"]["state"]["type"], "Closed");

    // The recording so far, as `Save bug report` downloads it.
    let (status, body) = get(port, &format!("/recording?token={token}")).await;
    assert_eq!(status, 200);
    let text = String::from_utf8(body).unwrap();
    assert!(text.starts_with(r#"{"caf_recording":3,"#), "{text}");
    assert!(text.ends_with('\n'));

    // Shutting down ends the chat, archived but still watched, and
    // `serve`, which closes the socket.
    running.caf.shutdown().await;
    let Debugger {
        live, dir: _dir, ..
    } = debugger;
    live.running.await;
    live.book.all_in().await;
    let running = &live.book;
    // The list of open chats, or the agents' statuses, may still be on
    // their way: each comes on its own, so either can trail the chat's
    // end. So may the chat as shutting down left it.
    let mut ended = false;
    let closed = loop {
        match next(&mut viewer).await {
            Ok(message) if message["type"] == "Chats" || message["type"] == "Agents" => {}
            Ok(message)
                if message["type"] == "Ended" && message["value"]["chat"] == chat && !ended =>
            {
                ended = true;
            }
            Ok(message)
                if (message["type"] == "Picture" || message["type"] == "Changes") && !ended =>
            {
                let id = chat.as_str().expect("an id");
                assert!(message.to_string().contains(id), "{message}");
            }
            other => break other,
        }
    };
    assert!(ended, "the watched chat's end comes first");
    assert_eq!(closed, Err(Some(1000)), "closed, normally");
    // The recording check checked every step.
    assert_eq!(running.recording_checked(), running.end());

    // The finished recording replays to the same directives, but for
    // what a recording hides: here, the fake's arguments, which name its
    // script, as it's started and as its status command runs it, before
    // its profile's own. Within budget, too, even in a debug build.
    fn hidden(directives: &Value) -> Value {
        let mut directives = directives.clone();
        for directive in directives.as_array_mut().unwrap() {
            if directive["value"]["type"] == "Start" {
                directive["value"]["value"]["command"]["args"] = json!([]);
            }
            if directive["value"]["type"] == "Status" {
                let args = &mut directive["value"]["value"]["command"]["args"];
                args.as_array_mut().unwrap().remove(0);
            }
        }
        directives
    }
    let end = running.end();
    seen.steps_to(&mut inspector, end).await;
    let replayed = replay::load(&recording).expect("the recording loads");
    assert_eq!(replayed.end(), end, "every step recorded");
    for (live, again) in seen.steps.iter().zip(replayed.after(0)) {
        let again = serde_json::to_value(&*again).unwrap();
        assert_eq!(live["event"], again["event"], "step {}", again["no"]);
        assert_eq!(
            hidden(&live["directives"]),
            again["directives"],
            "step {}",
            again["no"]
        );
    }
    let started = Instant::now();
    let mut player = Player::new(Arc::clone(&replayed));
    player.go(end);
    let took = started.elapsed();
    assert!(took < BUDGET, "replaying {end} steps took {took:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_fake_while_it_asks_ends_the_turn_and_it_answers_the_next() {
    let debugger = Debugger::start("stop").await;
    let mut viewer = debugger.chat().await;
    let chat = viewer.chat.clone();
    viewer.say(fake::ASK).await;
    viewer.until(|chat| !chat.questions.is_empty()).await;
    viewer.request("Stop", json!({"chat": chat})).await;
    viewer
        .until(|chat| lives(chat) == ["Stopped"] && chat.questions.is_empty())
        .await;
    // Nothing more comes of the stopped turn, and the next message is
    // answered as any is.
    viewer.say("hello again").await;
    viewer
        .until(|chat| lives(chat) == ["Stopped", "Finished"])
        .await;
    let words = serde_json::to_value(&viewer.copy.chat().turns[1].items).unwrap();
    assert!(words.to_string().contains("fake agent"), "{words:#}");
    assert!(debugger.live.book.diverged().is_empty());
    debugger.live.caf.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fake_fails_a_turn_once_and_answers_it_sent_again() {
    let debugger = Debugger::start("fail").await;
    let mut viewer = debugger.chat().await;
    viewer.say(fake::FAIL).await;
    viewer.until(|chat| lives(chat) == ["Failed"]).await;
    let life = serde_json::to_value(&viewer.copy.chat().turns[0].life).unwrap();
    assert_eq!(life["value"]["remedy"]["type"], "SendAgain", "{life:#}");
    assert!(
        life.to_string().contains("as it was asked to"),
        "the agent's words: {life:#}"
    );
    // Send again: answered this time.
    viewer.say(fake::FAIL).await;
    viewer
        .until(|chat| lives(chat) == ["Failed", "Finished"])
        .await;
    assert!(debugger.live.book.diverged().is_empty());
    debugger.live.caf.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_fake_limits_the_fakes_climb_turn_by_turn_and_limit_refuses_one() {
    let now = SystemTime::now();
    let debugger = Debugger::start_fake("limits", None, Some(now)).await;
    let kept = debugger.dir.0.join("usage/limits/caf-fake-agent.json");
    let mut viewer = debugger.chat().await;
    let share =
        |limits: &AccountLimits, kind: &WindowKind| limits.window(kind).map(|window| window.used);
    viewer.say("hello").await;
    viewer.until(|chat| lives(chat) == ["Finished"]).await;
    viewer
        .until_limits(|limits| share(limits, &WindowKind::FiveHours) == Some(60))
        .await;
    viewer.say("hello").await;
    viewer
        .until_limits(|limits| share(limits, &WindowKind::FiveHours) == Some(67))
        .await;
    viewer.until(|chat| lives(chat) == ["Finished"; 2]).await;
    let limits = viewer
        .agents
        .iter()
        .find(|agent| agent.name.0 == "fake")
        .unwrap()
        .limits
        .clone();
    assert_eq!(share(&limits, &WindowKind::Week), Some(32));
    assert_eq!(
        share(&limits, &WindowKind::ModelWeek("opus".into())),
        Some(21)
    );
    let five_hours = limits.window(&WindowKind::FiveHours).unwrap();
    // Its reset, in seconds, three hours on.
    let resets = five_hours.resets.unwrap().0;
    let expected = i64::try_from(
        (now + Duration::from_secs(3 * 3600))
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
        * 1000;
    assert_eq!(resets, expected);
    // `limit`: the five hours refused, and the turn failed.
    viewer.say(fake::LIMIT).await;
    viewer
        .until_limits(|limits| {
            limits
                .window(&WindowKind::FiveHours)
                .is_some_and(|window| window.state == LimitState::Refused && window.used == 100)
        })
        .await;
    viewer
        .until(|chat| lives(chat) == ["Finished", "Finished", "Failed"])
        .await;
    let life = serde_json::to_value(&viewer.copy.chat().turns[2].life).unwrap();
    assert_eq!(life["value"]["reason"]["type"], "UsageLimit", "{life:#}");
    assert_eq!(life["value"]["remedy"]["type"], "SendAgain", "{life:#}");
    // Only the five hours changed: the week isn't taken back.
    let limits = &viewer
        .agents
        .iter()
        .find(|agent| agent.name.0 == "fake")
        .unwrap()
        .limits;
    assert_eq!(share(limits, &WindowKind::Week), Some(32));
    // Sent again, answered, and fine again.
    viewer.say(fake::LIMIT).await;
    viewer
        .until_limits(|limits| {
            limits
                .window(&WindowKind::FiveHours)
                .is_some_and(|window| window.state != LimitState::Refused)
        })
        .await;
    viewer
        .until(|chat| lives(chat) == ["Finished", "Finished", "Failed", "Finished"])
        .await;
    assert!(debugger.live.book.diverged().is_empty());
    debugger.live.caf.shutdown().await;
    // Kept where every CAF app reads them, under the fake's profile.
    assert!(kept.exists(), "{}", kept.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_fake_limits_the_fake_says_none() {
    let debugger = Debugger::start("no-limits").await;
    let kept = debugger.dir.0.join("usage/limits/caf-fake-agent.json");
    let mut viewer = debugger.chat().await;
    viewer.say(fake::LIMIT).await;
    // `limit` is any message without them.
    viewer.until(|chat| lives(chat) == ["Finished"]).await;
    viewer.say("hello").await;
    viewer.until(|chat| lives(chat) == ["Finished"; 2]).await;
    let fake = (viewer.agents.iter())
        .find(|agent| agent.name.0 == "fake")
        .expect("the fake's status");
    assert_eq!(fake.limits, AccountLimits::default());
    assert!(debugger.live.book.diverged().is_empty());
    debugger.live.caf.shutdown().await;
    // None kept, even as the made-up ones are.
    assert!(!kept.exists(), "{}", kept.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fake_wants_a_sign_in_once_and_answers_it_sent_again() {
    let debugger = Debugger::start("sign-in").await;
    let mut viewer = debugger.chat().await;
    viewer.say(fake::SIGN_IN).await;
    viewer.until(|chat| lives(chat) == ["Failed"]).await;
    let life = serde_json::to_value(&viewer.copy.chat().turns[0].life).unwrap();
    assert_eq!(life["value"]["reason"]["type"], "SignInNeeded", "{life:#}");
    viewer.say(fake::SIGN_IN).await;
    viewer
        .until(|chat| lives(chat) == ["Failed", "Finished"])
        .await;
    assert!(debugger.live.book.diverged().is_empty());
    debugger.live.caf.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_fake_while_it_asks_always_ends_the_turn_stopped() {
    // CAF answers the question `cancelled` before it sends the cancel, so
    // a fake that went on once its question was answered would race its
    // own `cancelled` with `end_turn`. Many rounds in one chat give the
    // race every chance.
    const ROUNDS: usize = 30;
    let debugger = Debugger::start("stop-race").await;
    let mut viewer = debugger.chat().await;
    let chat = viewer.chat.clone();
    for round in 1..=ROUNDS {
        viewer.say(fake::ASK).await;
        viewer
            .until(|chat| chat.turns.len() == round && !chat.questions.is_empty())
            .await;
        viewer.request("Stop", json!({"chat": chat})).await;
        viewer
            .until(|chat| {
                let lives = lives(chat);
                lives.len() == round
                    && lives[round - 1] != "Running"
                    && lives[round - 1] != "Stopping"
            })
            .await;
        assert_eq!(
            lives(viewer.copy.chat())[round - 1],
            "Stopped",
            "round {round}"
        );
    }
    assert!(debugger.live.book.diverged().is_empty());
    debugger.live.caf.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_recording_check_passes_with_secrets_the_recording_hides() {
    use caf_core::agent::{EnvChange, McpServer};
    use caf_core::chat::UserMessage;
    let dir = Dir::new("secrets");
    let steps = [
        json!({"on": {"fallback": true, "do": [{"respond": {"error": {"code": -32601, "message": "Method not found"}}}]}}),
        json!({"wait": {"method": "initialize"}}),
        json!({"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {"mcpCapabilities": {"http": true}}, "agentInfo": {"name": "caf-fake-agent", "version": "1.0.0"}}}}),
        json!({"wait": {"method": "session/new"}}),
        json!({"respond": {"result": {"sessionId": "s1"}}}),
        json!({"notify": {"method": "_auth/status_update", "params": {"authStatus": {"kind": "account", "email": "me@example.com"}}}}),
        json!({"wait": {"method": "session/prompt"}}),
        json!({"say": {"session": "s1", "text": "Hi."}}),
        json!({"respond": {"result": {"stopReason": "end_turn"}}}),
    ];
    let script = dir.0.join("script.jsonl");
    let text: Vec<String> = steps.iter().map(Value::to_string).collect();
    std::fs::write(&script, text.join("\n") + "\n").unwrap();
    let mut command = SpawnCommand::new(caf_fake_agent::binary());
    command.args = vec![script.into_os_string()];
    command.env = vec![EnvChange::Set("AGENT_SECRET".into(), "s3cret-agent".into())];
    let agents = vec![AgentSpec::command(
        "fake",
        command,
        ProfileChoice::Builtin(Builtin::Fake),
    )];
    let recording = dir.0.join("run.jsonl");
    let live = live::start(agents, Some(recording.clone()), None, ledger(&dir));
    assert_eq!(live.recording.as_ref(), Some(&recording));

    let mut open = caf_core::OpenChat::new("fake", &dir.0);
    let mut tool = SpawnCommand::new("/bin/true");
    tool.env = vec![EnvChange::Set("TOKEN".into(), "s3cret-env".into())];
    open.mcp_servers = vec![
        McpServer::Stdio {
            name: "gh".into(),
            command: tool,
        },
        McpServer::Http {
            name: "maps".into(),
            url: "http://127.0.0.1:9/mcp".into(),
            headers: vec![("Authorization".into(), "Bearer s3cret-header".into())],
        },
    ];
    let chat = live.caf.open_chat(open).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hello")).await.unwrap();
    let mut seen: Option<Chat> = None;
    while seen.as_ref().is_none_or(|chat| lives(chat) != ["Finished"]) {
        let update = tokio::time::timeout(PATIENCE, watch.next()).await;
        match update
            .expect("the turn finishes")
            .expect("the chat is open")
        {
            caf_driver_tokio::Update::Picture(whole) => seen = Some(whole),
            caf_driver_tokio::Update::Changes(changes) => {
                let chat = seen.as_mut().expect("a picture first");
                for numbered in &changes {
                    chat.apply(&numbered.change).expect("it fits");
                }
            }
            _ => {}
        }
    }
    live.caf.shutdown().await;
    let Live { book, running, .. } = live;
    running.await;
    tokio::time::timeout(PATIENCE, book.all_in())
        .await
        .expect("every step is in");

    // The secrets went to the agent: in the steps' events and directives.
    let steps = serde_json::to_string(&book.after(0)).unwrap();
    let holds = |text: &str, secret: &str| {
        let bytes = serde_json::to_string(secret.as_bytes()).unwrap();
        text.contains(secret) || text.contains(bytes.trim_matches(['[', ']']))
    };
    let recorded = std::fs::read_to_string(&recording).unwrap();
    for secret in [
        "s3cret-env",
        "s3cret-header",
        "me@example.com",
        "s3cret-agent",
    ] {
        assert!(holds(&steps, secret), "the steps hold {secret}");
        assert!(!holds(&recorded, secret), "the recording hides {secret}");
    }
    // And yet both replays agree with CAF, every step checked.
    assert!(book.diverged().is_empty(), "{:?}", book.diverged());
    assert_eq!(book.recording_checked(), book.end());
    assert!(book.end() > 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_that_couldnt_be_made_is_none() {
    let dir = Dir::new("no-recording");
    let script = fake::write(&dir.0, false, None).unwrap();
    let mut command = SpawnCommand::new(caf_fake_agent::binary());
    command.args = vec![script.into_os_string()];
    let agents = vec![AgentSpec::command(
        "fake",
        command,
        ProfileChoice::Builtin(Builtin::Fake),
    )];
    let live = live::start(
        agents,
        Some(dir.0.join("no/such/folder/run.jsonl")),
        None,
        ledger(&dir),
    );
    assert!(live.recording.is_none());
    assert!(live.book.file.is_none(), "Hello says there's none");
    live.caf.shutdown().await;
    live.running.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_saved_live_is_restored_by_the_next_debugger_and_carries_on() {
    let store = Dir::new("saving-chats");
    let first = Debugger::start_with("saving-first", Some(store.0.clone())).await;
    let mut viewer = first.chat().await;
    let chat = viewer.chat.clone();
    viewer.say("hello").await;
    viewer.until(|chat| lives(chat) == ["Finished"]).await;
    let chats = first.live.caf.watch_chats().next().await.unwrap();
    assert_eq!(serde_json::to_value(chats[0].id).unwrap(), chat);
    assert_eq!(
        chats[0].shelf,
        Some(live::shelf()),
        "kept on the debugger's shelf"
    );
    first.stop().await;

    // The next debugger lists it, and restores it when asked.
    let second = Debugger::start_with("saving-second", Some(store.0.clone())).await;
    let mut socket = open(second.port, "/viewer", &second.token).await;
    send(
        &mut socket,
        json!({"type": "Hello", "value": {"min": 1, "max": 1}}),
    )
    .await;
    let saved = viewer_until(&mut socket, |message| {
        message["type"] == "Saved"
            && message["value"]
                .as_array()
                .is_some_and(|saved| !saved.is_empty())
    })
    .await;
    let listed = &saved["value"][0];
    assert_eq!(listed["header"]["chat"], chat);
    assert_eq!(listed["shelf"], live::SHELF);
    assert_eq!(listed["state"], json!({"type": "Saved"}));
    assert_eq!(listed["header"]["first"], "hello");
    let restore = json!({"request": 1, "chat": chat, "shelf": "elsewhere"});
    send(&mut socket, json!({"type": "Restore", "value": restore})).await;
    let done = viewer_until(&mut socket, |message| message["type"] == "Done").await;
    assert_eq!(
        done["value"]["outcome"]["type"], "NotAllowed",
        "only its own shelf"
    );
    let restore = json!({"request": 2, "chat": chat, "shelf": live::SHELF});
    send(&mut socket, json!({"type": "Restore", "value": restore})).await;
    let done = viewer_until(&mut socket, |message| message["type"] == "Done").await;
    assert_eq!(
        done["value"]["outcome"],
        json!({"type": "Opened", "value": chat}),
        "{done}"
    );
    send(
        &mut socket,
        json!({"type": "Watch", "value": {"chat": chat, "since": null}}),
    )
    .await;
    let picture = viewer_until(&mut socket, |message| message["type"] == "Picture").await;
    let ToViewer::Picture {
        chat: whole,
        latest,
    } = serde_json::from_value(picture).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(lives(&whole), ["Finished"], "its record");
    let mut viewer = Viewer {
        socket,
        chat,
        copy: ChatCopy::new(whole, latest),
        requests: 2,
        agents: Vec::new(),
    };
    viewer.say("again").await;
    viewer
        .until(|chat| lives(chat) == ["Finished", "Finished"])
        .await;
    // The fake resumes the session, so the chat carries on with no note.
    let notes = (viewer.copy.chat().turns.iter())
        .flat_map(|turn| &turn.items)
        .filter(|item| matches!(item.kind, caf_core::chat::ItemKind::Note(_)))
        .count();
    assert_eq!(notes, 0, "{:#?}", viewer.copy.chat());
    second.stop().await;
}
