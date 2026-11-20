//! The server over real sockets: what's locked and how, and the
//! inspector channel end to end, in replay.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use caf_debugger::inspector::{FromInspector, Mode, PROTOCOL, ToInspector};
use caf_debugger::replay;
use caf_debugger::server::{self, App, INSPECTOR_MESSAGE};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn claude_live() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/src/core/tests/traces/claude-0.98.0-live.jsonl")
}

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

/// A server replaying the live trace, changed by `change`.
struct Server {
    port: u16,
    token: String,
}

impl Server {
    async fn start(change: impl FnOnce(&mut App)) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let book = replay::load(&claude_live()).expect("the trace loads");
        let token = server::new_token();
        let mut app = App::new(book, port, token.clone());
        app.dist = PathBuf::from("/nonexistent");
        change(&mut app);
        let router = server::router(Arc::new(app));
        tokio::spawn(async move { axum::serve(listener, router).await });
        Self { port, token }
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Opens a WebSocket at `path`, with this query and Origin.
    async fn socket(&self, path: &str, query: &str, origin: Option<&str>) -> Result<Socket, Error> {
        self.socket_as(path, query, origin, None).await
    }

    /// Opens a WebSocket, naming `host` as the server if given.
    async fn socket_as(
        &self,
        path: &str,
        query: &str,
        origin: Option<&str>,
        host: Option<&str>,
    ) -> Result<Socket, Error> {
        let url = format!("ws://127.0.0.1:{}{path}{query}", self.port);
        let mut request = url.into_client_request().unwrap();
        if let Some(origin) = origin {
            (request.headers_mut()).insert("Origin", HeaderValue::from_str(origin).unwrap());
        }
        if let Some(host) = host {
            (request.headers_mut()).insert("Host", HeaderValue::from_str(host).unwrap());
        }
        tokio_tungstenite::connect_async(request)
            .await
            .map(|(socket, _)| socket)
    }

    /// The inspector, as the app opens it.
    async fn inspector(&self) -> Socket {
        let query = format!("?token={}", self.token);
        self.socket("/inspect", &query, Some(&self.origin()))
            .await
            .expect("the inspector opens")
    }

    /// A plain GET: its status, headers and body.
    async fn get(&self, target: &str, origin: Option<&str>) -> (u16, String, Vec<u8>) {
        self.get_as(target, origin, &format!("127.0.0.1:{}", self.port))
            .await
    }

    /// A plain GET, naming `host` as the server.
    async fn get_as(
        &self,
        target: &str,
        origin: Option<&str>,
        host: &str,
    ) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        let origin = origin.map_or(String::new(), |origin| format!("Origin: {origin}\r\n"));
        let request =
            format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n{origin}Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let split = (response.windows(4))
            .position(|window| window == b"\r\n\r\n")
            .expect("a whole response");
        let head = String::from_utf8(response[..split].to_vec()).unwrap();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, head.to_lowercase(), response[split + 4..].to_vec())
    }
}

/// The status a refused WebSocket got.
fn refused(result: Result<Socket, Error>) -> u16 {
    match result {
        Err(Error::Http(response)) => response.status().as_u16(),
        Err(other) => panic!("not refused by HTTP: {other}"),
        Ok(_) => panic!("not refused"),
    }
}

/// The next message from the server.
async fn next(socket: &mut Socket) -> ToInspector {
    let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("a message comes")
        .expect("the socket is open")
        .expect("a message");
    let Message::Text(text) = message else {
        panic!("not text: {message:?}");
    };
    serde_json::from_str(&text).expect("an inspector message")
}

/// Sends the app's message.
async fn send(socket: &mut Socket, message: &FromInspector) {
    let text = serde_json::to_string(message).unwrap();
    socket.send(Message::text(text)).await.unwrap();
}

/// How the server closed the socket: its close code, if it gave one.
async fn closed(socket: &mut Socket) -> Option<CloseCode> {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("the socket closes");
        match message {
            Some(Ok(Message::Close(frame))) => return frame.map(|frame| frame.code),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return None,
        }
    }
}

/// The next message, which must be a Past at `at`, as JSON.
async fn past(socket: &mut Socket, at: u64) -> Value {
    match next(socket).await {
        ToInspector::Past { at: was, message } => {
            assert_eq!(was, at);
            serde_json::to_value(message).unwrap()
        }
        other => panic!("not a Past: {other:?}"),
    }
}

#[tokio::test]
async fn the_inspector_needs_the_token_and_the_servers_own_origin() {
    let server = Server::start(|_| {}).await;
    let own = server.origin();
    let token = format!("?token={}", server.token);
    let wrong = format!("?token={}", server::new_token());
    let short = format!("?token={}", &server.token[..10]);
    let open = |query: String, origin: Option<String>| {
        let server = &server;
        async move { server.socket("/inspect", &query, origin.as_deref()).await }
    };
    assert_eq!(refused(open(String::new(), Some(own.clone())).await), 403);
    assert_eq!(refused(open(wrong, Some(own.clone())).await), 403);
    assert_eq!(refused(open(short, Some(own.clone())).await), 403);
    assert_eq!(
        refused(open(token.clone(), None).await),
        403,
        "an Origin is needed"
    );
    let evil = Some("http://evil.example".to_owned());
    assert_eq!(refused(open(token.clone(), evil).await), 403);
    let other_port = Some(format!("http://127.0.0.1:{}", server.port.wrapping_add(1)));
    assert_eq!(refused(open(token.clone(), other_port).await), 403);
    let null = Some("null".to_owned());
    assert_eq!(refused(open(token.clone(), null).await), 403);
    let longer = Some(format!("http://127.0.0.1:{}.evil.com", server.port));
    assert_eq!(refused(open(token.clone(), longer).await), 403);
    // Its own Origin, but another name for the server: DNS rebinding.
    let rebound = format!("evil.example:{}", server.port);
    let rebound = server
        .socket_as("/inspect", &token, Some(&own), Some(&rebound))
        .await;
    assert_eq!(refused(rebound), 403);
    let localhost = Some(format!("http://localhost:{}", server.port));
    let mut socket = open(token.clone(), localhost)
        .await
        .expect("localhost is its own too");
    assert!(matches!(
        next(&mut socket).await,
        ToInspector::Hello {
            mode: Mode::Replay,
            ..
        }
    ));
    let mut socket = open(token, Some(own)).await.expect("it opens");
    assert!(matches!(next(&mut socket).await, ToInspector::Hello { .. }));
}

#[tokio::test]
async fn the_inspector_sends_the_replay_and_answers_what_the_app_asks() {
    let server = Server::start(|_| {}).await;
    let mut socket = server.inspector().await;
    let ToInspector::Hello {
        protocol,
        mode,
        file,
        run,
    } = next(&mut socket).await
    else {
        panic!("Hello first");
    };
    assert_eq!(protocol, PROTOCOL);
    assert_eq!(mode, Mode::Replay);
    assert_eq!(file, Some(claude_live().display().to_string()));
    assert_eq!(run, None);
    let ToInspector::Steps { from, steps } = next(&mut socket).await else {
        panic!("then the steps");
    };
    assert_eq!(from, 1);
    assert_eq!(steps.len(), 71);
    assert!(steps.iter().zip(1..).all(|(step, no)| step.no == no));
    let event: Value = serde_json::from_str(steps[0].event.get()).unwrap();
    assert_eq!(event["type"], "Host", "the first event opens the chat");
    let directives: Value = serde_json::from_str(steps[0].directives.get()).unwrap();
    assert!(directives.as_array().is_some_and(|all| !all.is_empty()));
    assert!(matches!(
        next(&mut socket).await,
        ToInspector::Position {
            live: false,
            at: 71,
            end: 71
        }
    ));
    // The chats at the end, whole.
    assert_eq!(past(&mut socket, 71).await["type"], "Chats");
    let picture = past(&mut socket, 71).await;
    assert_eq!(picture["type"], "Picture");
    assert_eq!(picture["value"]["chat"]["id"], 1);

    // Back: whole again.
    send(&mut socket, &FromInspector::Seek { step: 3 }).await;
    assert!(matches!(
        next(&mut socket).await,
        ToInspector::Position {
            live: false,
            at: 3,
            end: 71
        }
    ));
    assert_eq!(past(&mut socket, 3).await["type"], "Chats");
    assert_eq!(past(&mut socket, 3).await["type"], "Picture");
    // Forward, past the end: changes, up to the end.
    send(&mut socket, &FromInspector::Seek { step: 1000 }).await;
    assert!(matches!(
        next(&mut socket).await,
        ToInspector::Position {
            live: false,
            at: 71,
            end: 71
        }
    ));
    assert_eq!(past(&mut socket, 71).await["type"], "Chats");
    assert_eq!(past(&mut socket, 71).await["type"], "Changes");
    // Following, in replay, changes nothing.
    send(&mut socket, &FromInspector::Follow).await;
    assert!(matches!(
        next(&mut socket).await,
        ToInspector::Position {
            live: false,
            at: 71,
            end: 71
        }
    ));
    // The state, at the start and at the end.
    send(&mut socket, &FromInspector::StateAt { step: 0 }).await;
    let ToInspector::State { at: 0, core } = next(&mut socket).await else {
        panic!("the state at 0");
    };
    assert_eq!(core["chats"], json!([]));
    send(&mut socket, &FromInspector::StateAt { step: 71 }).await;
    let ToInspector::State { at: 71, core } = next(&mut socket).await else {
        panic!("the state at 71");
    };
    assert_eq!(core["chats"][0]["state"]["type"], "Lost");
}

#[tokio::test]
async fn what_the_app_sends_wrong_or_too_big_closes_the_inspector() {
    let server = Server::start(|_| {}).await;
    let mut socket = server.inspector().await;
    socket.send(Message::text("not JSON")).await.unwrap();
    assert_eq!(closed(&mut socket).await, Some(CloseCode::Invalid));

    let mut socket = server.inspector().await;
    let unknown = json!({"type": "Rewind"}).to_string();
    socket.send(Message::text(unknown)).await.unwrap();
    assert_eq!(closed(&mut socket).await, Some(CloseCode::Invalid));

    let mut socket = server.inspector().await;
    socket.send(Message::binary(vec![0; 8])).await.unwrap();
    assert_eq!(closed(&mut socket).await, Some(CloseCode::Unsupported));

    let mut socket = server.inspector().await;
    let big = json!({"type": "Seek", "value": {"step": 1, "pad": "x".repeat(INSPECTOR_MESSAGE)}});
    socket.send(Message::text(big.to_string())).await.unwrap();
    assert_eq!(closed(&mut socket).await, Some(CloseCode::Size));
}

#[tokio::test]
async fn the_viewer_is_live_only_and_locked_too() {
    let server = Server::start(|_| {}).await;
    let own = server.origin();
    let token = format!("?token={}", server.token);
    assert_eq!(refused(server.socket("/viewer", "", Some(&own)).await), 403);
    assert_eq!(refused(server.socket("/viewer", &token, None).await), 403);
    assert_eq!(
        refused(
            server
                .socket("/viewer", &token, Some("http://evil.example"))
                .await
        ),
        403
    );
    assert_eq!(
        refused(server.socket("/viewer", &token, Some(&own)).await),
        404,
        "replaying, there's nothing live"
    );
}

#[tokio::test]
async fn the_recording_downloads_in_whole_lines_with_the_token() {
    let dir = Dir::new("recording");
    let path = dir.0.join("2026-11-20T10-00-00Z.jsonl");
    std::fs::write(&path, "{\"caf_recording\":2}\n{\"now\":").unwrap();
    let recording = path.clone();
    let server = Server::start(move |app| app.recording = Some(recording)).await;
    let target = format!("/recording?token={}", server.token);
    let (status, head, body) = server.get(&target, None).await;
    assert_eq!(status, 200, "a download link sends no Origin");
    assert_eq!(body, b"{\"caf_recording\":2}\n");
    assert!(
        head.contains(
            "content-disposition: attachment; filename=\"caf-2026-11-20t10-00-00z.jsonl\""
        )
    );
    assert!(!head.contains("access-control-allow-origin"), "no CORS");
    assert_eq!(server.get(&target, Some(&server.origin())).await.0, 200);
    assert_eq!(
        server.get(&target, Some("http://evil.example")).await.0,
        403
    );
    assert_eq!(server.get("/recording", None).await.0, 403);
    let wrong = format!("/recording?token={}", server::new_token());
    assert_eq!(server.get(&wrong, None).await.0, 403);
    let rebound = format!("evil.example:{}", server.port);
    assert_eq!(
        server.get_as(&target, None, &rebound).await.0,
        403,
        "another name"
    );

    let replaying = Server::start(|_| {}).await;
    let target = format!("/recording?token={}", replaying.token);
    assert_eq!(
        replaying.get(&target, None).await.0,
        404,
        "nothing recorded"
    );
}

#[tokio::test]
async fn the_app_is_served_from_its_folder_or_says_how_to_build_it() {
    let server = Server::start(|_| {}).await;
    let (status, head, body) = server.get("/", None).await;
    assert_eq!(status, 503);
    assert!(head.contains("content-type: text/html"));
    assert!(String::from_utf8(body).unwrap().contains("isn't built"));
    assert!(head.contains("content-security-policy: default-src 'self'"));

    let dir = Dir::new("dist");
    std::fs::create_dir_all(dir.0.join("assets")).unwrap();
    std::fs::write(dir.0.join("index.html"), "<!doctype html>app").unwrap();
    std::fs::write(dir.0.join("assets/app.js"), "let a = 1;").unwrap();
    std::fs::write(
        dir.0.parent().unwrap().join("caf-debugger-secret"),
        "secret",
    )
    .unwrap();
    let dist = dir.0.clone();
    let server = Server::start(move |app| app.dist = dist).await;
    let (status, head, body) = server.get("/", None).await;
    assert_eq!(
        (status, body.as_slice()),
        (200, b"<!doctype html>app".as_slice())
    );
    assert!(head.contains("content-type: text/html"));
    assert!(head.contains("x-content-type-options: nosniff"));
    let localhost = format!("localhost:{}", server.port);
    assert_eq!(server.get_as("/", None, &localhost).await.0, 200);
    let rebound = format!("evil.example:{}", server.port);
    assert_eq!(
        server.get_as("/", None, &rebound).await.0,
        403,
        "DNS rebinding"
    );
    assert_eq!(
        server.get_as("/", None, "127.0.0.1").await.0,
        403,
        "no port"
    );
    let csp = format!("content-security-policy: {}", server::CSP.to_lowercase());
    assert!(head.contains(&csp), "{head}");
    assert!(
        server::CSP.contains("default-src 'self'")
            && server::CSP.contains("frame-ancestors 'none'")
    );
    assert!(!head.contains("access-control-allow-origin"), "no CORS");
    let (status, head, _) = server.get("/assets/app.js", None).await;
    assert_eq!(status, 200);
    assert!(head.contains("content-type: text/javascript"));
    assert!(head.contains(&csp), "{head}");
    let (status, head, _) = server.get("/assets/missing.js", None).await;
    assert_eq!(status, 404);
    assert!(head.contains(&csp) && head.contains("x-content-type-options: nosniff"));
    assert_eq!(server.get("/../caf-debugger-secret", None).await.0, 404);
    assert_eq!(
        server
            .get("/assets/%2e%2e/%2e%2e/caf-debugger-secret", None)
            .await
            .0,
        404
    );
    let _ = std::fs::remove_file(dir.0.parent().unwrap().join("caf-debugger-secret"));
}

#[tokio::test]
async fn a_big_recording_downloads_whole_as_it_is_read() {
    let dir = Dir::new("big-recording");
    let path = dir.0.join("big.jsonl");
    // Lines of many lengths around the size read at once, then half a
    // line.
    let mut text = String::new();
    let mut n = 0;
    while text.len() < 3 << 20 {
        text += &format!(
            "{{\"line\":{n},\"pad\":\"{}\"}}\n",
            "x".repeat(n * 7919 % 70_000)
        );
        n += 1;
    }
    let whole = text.clone();
    text += "{\"half\":";
    std::fs::write(&path, &text).unwrap();
    let recording = path.clone();
    let server = Server::start(move |app| app.recording = Some(recording)).await;
    let target = format!("/recording?token={}", server.token);
    let (status, head, got) = server.get(&target, None).await;
    assert_eq!(status, 200);
    assert!(
        head.contains(&format!("content-length: {}", whole.len())),
        "{head}"
    );
    assert_eq!(got.len(), whole.len());
    assert!(got == whole.as_bytes(), "the file up to its last newline");

    // No newline at all: nothing.
    std::fs::write(&path, "{\"half\":").unwrap();
    let (status, _, body) = server.get(&target, None).await;
    assert_eq!(status, 200);
    assert!(body.is_empty());
}

#[tokio::test]
async fn of_seeks_waiting_together_only_the_last_is_answered_but_every_state_is() {
    let server = Server::start(|_| {}).await;
    let mut socket = server.inspector().await;
    // Hello, Steps, Position, and the Past at the end, the Picture last.
    loop {
        if let ToInspector::Past { message, .. } = next(&mut socket).await
            && serde_json::to_value(&message).unwrap()["type"] == "Picture"
        {
            break;
        }
    }
    // A drag across the timeline, with the state asked for at each step,
    // and twice at the last, as the State tab asks for a step and the one
    // before it.
    for step in 1..=60 {
        let seek = serde_json::to_string(&FromInspector::Seek { step }).unwrap();
        let state = serde_json::to_string(&FromInspector::StateAt { step }).unwrap();
        socket.feed(Message::text(seek)).await.unwrap();
        socket.feed(Message::text(state)).await.unwrap();
    }
    let again = serde_json::to_string(&FromInspector::StateAt { step: 60 }).unwrap();
    socket.feed(Message::text(again)).await.unwrap();
    socket.flush().await.unwrap();
    let mut positions = Vec::new();
    let mut states = Vec::new();
    while states.len() < 61 {
        match next(&mut socket).await {
            ToInspector::Position { at, .. } => positions.push(at),
            ToInspector::State { at, core } => {
                assert_eq!(
                    core["chats"].as_array().map(Vec::len),
                    Some(usize::from(at > 0))
                );
                states.push(at);
            }
            _ => {}
        }
    }
    assert_eq!(positions.last(), Some(&60), "the last seek is answered");
    assert!(positions.len() < 20, "not every seek: {positions:?}");
    let mut asked: Vec<u64> = (1..=60).collect();
    asked.push(60);
    assert_eq!(states, asked, "every state, once each, in order");
}
