//! The HTTP server, on 127.0.0.1 only.
//!
//! | Path | What |
//! |---|---|
//! | `/` | The app, from `ts/debugger/dist` |
//! | `/viewer` | A WebSocket of the viewer wire, in JSON, served by `serve`: live only |
//! | `/inspect` | A WebSocket of the inspector channel ([`crate::inspector`]) |
//! | `/recording` | The live run's recording, to download |
//!
//! The server starts agents in the user's folders, and any web page can
//! try to reach a port on 127.0.0.1, so everything but the app itself is
//! locked:
//!
//! - **A token**, new each launch, given in the URL's fragment
//!   (`#token=…`), which browsers never send, so it stays out of logs and
//!   `Referer`s. The app passes it on as `?token=`, which is checked in
//!   constant time.
//! - **The Host** must be the server's own, `127.0.0.1:PORT` or
//!   `localhost:PORT`, on every path, the app's too: a page whose name
//!   was pointed at 127.0.0.1 reaches the port, but under its own name.
//! - **The Origin** must be the server's own, `http://127.0.0.1:PORT` or
//!   `http://localhost:PORT`: always on a WebSocket, which a browser
//!   always sends it with. `/recording` may also have none, as a
//!   download link or a page's own `fetch` sends none.
//! - **No CORS:** nothing here says another origin may read it.
//! - **A Content-Security-Policy** on the app ([`CSP`]): its page loads
//!   and connects to nothing but the server, and no other page may frame
//!   it. Every response has `X-Content-Type-Options: nosniff`.
//! - **What the app sends is capped:** [`VIEWER_MESSAGE`] on `/viewer`,
//!   [`INSPECTOR_MESSAGE`] on `/inspect`. A bigger one ends the socket.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use caf_core::viewer::FromViewer;
use caf_driver_tokio::Caf;
use caf_driver_tokio::viewer::{ViewerRules, serve};
use futures_util::FutureExt;
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::mpsc;

use crate::inspector::{Check, FromInspector, Mode, PROTOCOL, Step, ToInspector};
use crate::replay::Player;
use crate::steps::Book;

/// The biggest message the app may send on `/viewer`: a message to the
/// agent, with room to spare.
pub const VIEWER_MESSAGE: usize = 1 << 20;

/// The biggest message the app may send on `/inspect`, whose messages
/// are a few numbers each.
pub const INSPECTOR_MESSAGE: usize = 4 << 10;

/// How many steps go in one [`ToInspector::Steps`] at most.
const STEPS_AT_ONCE: usize = 500;

/// How big one [`ToInspector::Steps`] may get, in bytes of JSON, unless
/// one step alone is bigger.
const STEPS_BYTES: usize = 8 << 20;

/// How long to wait for the app's side of a closing handshake.
const CLOSING: Duration = Duration::from_secs(1);

/// Everything the server serves.
pub struct App {
    /// The steps.
    pub book: Arc<Book>,
    /// Live, CAF and what viewers may do with it.
    pub live: Option<(Caf, Arc<ViewerRules>)>,
    /// Live, the recording.
    pub recording: Option<PathBuf>,
    /// Whether CAF serves its own tools: false unless set, as in replay.
    pub tools: bool,
    /// This launch's token.
    pub token: String,
    /// The origins that count as the server's own.
    pub origins: [String; 2],
    /// The hosts that count as the server's own, as a `Host` header names
    /// them.
    pub hosts: [String; 2],
    /// The built app.
    pub dist: PathBuf,
}

impl App {
    /// What's served on `port`, with `token`.
    #[must_use]
    pub fn new(book: Arc<Book>, port: u16, token: String) -> Self {
        Self {
            book,
            live: None,
            recording: None,
            tools: false,
            token,
            origins: [
                format!("http://127.0.0.1:{port}"),
                format!("http://localhost:{port}"),
            ],
            hosts: [format!("127.0.0.1:{port}"), format!("localhost:{port}")],
            dist: dist(),
        }
    }
}

/// Where the built app is: `ts/debugger/dist` in CAF's repository.
#[must_use]
pub fn dist() -> PathBuf {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2);
    repository.map_or_else(PathBuf::new, |repository| {
        repository.join("ts/debugger/dist")
    })
}

/// A new token: 32 random bytes, in hex.
///
/// # Panics
///
/// If the system has no randomness to give.
#[must_use]
pub fn new_token() -> String {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).expect("the system gives randomness");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The URL to open, with the token in its fragment.
#[must_use]
pub fn url(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/#token={token}")
}

/// The routes.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/viewer", get(viewer))
        .route("/inspect", get(inspect))
        .route("/recording", get(recording))
        .fallback(get(static_file))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&app),
            own_host,
        ))
        .layer(axum::middleware::map_response(headers))
        .with_state(app)
}

/// Headers on everything: no guessing types, no `Referer`, no caching.
async fn headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Refuses a request not addressed to the server by its own name, on
/// every path: a page whose own name was pointed at 127.0.0.1 (DNS
/// rebinding) reaches the port, but its requests name its own host.
async fn own_host(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let host = request.headers().get(header::HOST);
    let own = host.is_some_and(|host| {
        app.hosts
            .iter()
            .any(|own| host.as_bytes() == own.as_bytes())
    });
    if !own {
        return (
            StatusCode::FORBIDDEN,
            "not addressed to the debugger by its own name\n",
        )
            .into_response();
    }
    next.run(request).await
}

/// The token, as the app passes it on.
#[derive(Deserialize)]
struct Token {
    token: Option<String>,
}

/// Whether a request may go on: its token is this launch's, and its
/// Origin the server's own, or, if `none_ok`, missing. If not, the
/// answer refusing it.
fn allowed(app: &App, token: &Token, headers: &HeaderMap, none_ok: bool) -> Option<Response> {
    let given = token.token.as_deref().unwrap_or_default();
    if !bool::from(given.as_bytes().ct_eq(app.token.as_bytes())) {
        return Some((StatusCode::FORBIDDEN, "a wrong or missing token\n").into_response());
    }
    let origin_ok = match headers.get(header::ORIGIN) {
        None => none_ok,
        Some(origin) => app
            .origins
            .iter()
            .any(|own| origin.as_bytes() == own.as_bytes()),
    };
    (!origin_ok)
        .then(|| (StatusCode::FORBIDDEN, "not from the debugger's own page\n").into_response())
}

/// `/viewer`: the viewer wire, for the live CAF.
async fn viewer(
    State(app): State<Arc<App>>,
    Query(token): Query<Token>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if let Some(refused) = allowed(&app, &token, &headers, false) {
        return refused;
    }
    let Some((caf, rules)) = app.live.clone() else {
        return (
            StatusCode::NOT_FOUND,
            "replaying: there's no live CAF to view\n",
        )
            .into_response();
    };
    upgrade
        .max_message_size(VIEWER_MESSAGE)
        .max_frame_size(VIEWER_MESSAGE)
        .on_upgrade(move |socket| viewer_socket(socket, caf, rules))
}

/// Runs the viewer wire over one socket, until either side ends it.
async fn viewer_socket(mut socket: WebSocket, caf: Caf, rules: Arc<ViewerRules>) {
    let (to_caf, from) = mpsc::channel(16);
    let (to, mut to_viewer) = mpsc::channel(64);
    tokio::spawn(serve(caf, rules, from, to));
    // A message from the app waiting for room on `to_caf`. Meanwhile
    // nothing more is read from the app, but what CAF sends still goes
    // out, so `serve` is never left waiting for room itself.
    let mut waiting: Option<FromViewer> = None;
    loop {
        tokio::select! {
            message = to_viewer.recv() => {
                let Some(message) = message else {
                    // `serve` has ended the link.
                    close(socket, close_code::NORMAL, "").await;
                    return;
                };
                let text = serde_json::to_string(&message).expect("viewer messages serialise");
                if socket.send(Message::Text(text.into())).await.is_err() {
                    return;
                }
            }
            room = to_caf.reserve(), if waiting.is_some() => {
                let Ok(room) = room else { return };
                room.send(waiting.take().expect("a message waits"));
            }
            incoming = socket.recv(), if waiting.is_none() => match read::<FromViewer>(incoming) {
                Read::Message(message) => waiting = Some(message),
                Read::Skip => {}
                Read::Closed => return,
                Read::Bad(code, why) => {
                    close(socket, code, why).await;
                    return;
                }
            },
        }
    }
}

/// What the app sent.
enum Read<T> {
    /// A message.
    Message(T),
    /// Nothing to act on, such as a ping.
    Skip,
    /// The socket has closed.
    Closed,
    /// Something that isn't a message: close the socket with this code
    /// and reason.
    Bad(u16, &'static str),
}

/// Reads what the app sent as one of `T`.
fn read<T: for<'de> Deserialize<'de>>(incoming: Option<Result<Message, axum::Error>>) -> Read<T> {
    match incoming {
        Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
            Ok(message) => Read::Message(message),
            Err(_) => Read::Bad(close_code::INVALID, "not a message of this socket's"),
        },
        Some(Ok(Message::Binary(_))) => {
            Read::Bad(close_code::UNSUPPORTED, "messages are JSON text")
        }
        Some(Ok(Message::Ping(_) | Message::Pong(_))) => Read::Skip,
        Some(Err(error)) => failed(error),
        Some(Ok(Message::Close(_))) | None => Read::Closed,
    }
}

/// Why reading failed: a message too big, or one breaking the protocol,
/// is said so in the close; otherwise the connection has gone.
fn failed<T>(error: axum::Error) -> Read<T> {
    let error = error.into_inner();
    match error.downcast_ref::<tungstenite::Error>() {
        Some(tungstenite::Error::Capacity(_)) => Read::Bad(close_code::SIZE, "a message too big"),
        Some(tungstenite::Error::Protocol(_) | tungstenite::Error::Utf8(_)) => {
            Read::Bad(close_code::PROTOCOL, "not WebSocket")
        }
        _ => Read::Closed,
    }
}

/// Closes a socket properly: says why, then waits a moment for the app
/// to say it has closed too.
async fn close(mut socket: WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    if socket.send(Message::Close(Some(frame))).await.is_err() {
        return;
    }
    let _ = tokio::time::timeout(CLOSING, async {
        while let Some(Ok(message)) = socket.recv().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// `/inspect`: the inspector channel.
async fn inspect(
    State(app): State<Arc<App>>,
    Query(token): Query<Token>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if let Some(refused) = allowed(&app, &token, &headers, false) {
        return refused;
    }
    let book = Arc::clone(&app.book);
    let tools = app.tools;
    upgrade
        .max_message_size(INSPECTOR_MESSAGE)
        .max_frame_size(INSPECTOR_MESSAGE)
        .on_upgrade(move |socket| Inspector::new(socket, book, tools).run())
}

/// One app's inspector channel.
///
/// What the app asks for is answered in turn, but a move the app has
/// already moved on from isn't: of the messages waiting when it gets to
/// them, only the last move (a Seek or Follow) is answered, then every
/// StateAt, each with one State, in order. So dragging the timeline costs
/// one replay per answer, not one per step dragged over. Replaying runs
/// on a thread for blocking work, never on the threads CAF runs on.
struct Inspector {
    socket: WebSocket,
    book: Arc<Book>,
    /// Whether CAF serves its own tools.
    tools: bool,
    /// How many steps it has been sent.
    sent: u64,
    /// Following live.
    following: bool,
    /// The checks whose divergence it has been told of.
    told: Vec<Check>,
    /// The core at the position the app looks at: away on a blocking
    /// thread only while it moves.
    player: Option<Player>,
    /// The core whose state it last asked for, likewise.
    state: Option<Player>,
}

/// The socket has gone.
struct Gone;

impl Inspector {
    fn new(socket: WebSocket, book: Arc<Book>, tools: bool) -> Self {
        Self {
            socket,
            tools,
            player: Some(Player::new(Arc::clone(&book))),
            state: Some(Player::new(Arc::clone(&book))),
            following: book.mode == Mode::Live,
            book,
            sent: 0,
            told: Vec::new(),
        }
    }

    async fn run(mut self) {
        let mut end = self.book.watch_end();
        end.mark_unchanged();
        if self.start().await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                changed = end.changed() => {
                    if changed.is_err() || self.news().await.is_err() {
                        return;
                    }
                }
                incoming = self.socket.recv() => {
                    let mut asked = Vec::new();
                    let mut ended = None;
                    let mut next = Some(incoming);
                    // This message, and those waiting behind it.
                    while let Some(incoming) = next {
                        match read::<FromInspector>(incoming) {
                            Read::Message(message) => asked.push(message),
                            Read::Skip => {}
                            Read::Closed => {
                                ended = Some(None);
                                break;
                            }
                            Read::Bad(code, why) => {
                                ended = Some(Some((code, why)));
                                break;
                            }
                        }
                        next = self.socket.recv().now_or_never();
                    }
                    if self.answer(asked).await.is_err() {
                        return;
                    }
                    match ended {
                        Some(Some((code, why))) => {
                            close(self.socket, code, why).await;
                            return;
                        }
                        Some(None) => return,
                        None => {}
                    }
                }
            }
        }
    }

    /// What the app is sent as the link starts.
    async fn start(&mut self) -> Result<(), Gone> {
        let hello = ToInspector::Hello {
            protocol: PROTOCOL,
            mode: self.book.mode,
            file: self.book.file.clone(),
            run: self.book.run.clone(),
            tools: self.tools,
        };
        self.send(&hello).await?;
        self.steps().await?;
        let end = self.sent;
        self.position(end).await?;
        self.diverged().await?;
        if self.book.mode == Mode::Replay {
            self.past(end).await?;
        }
        Ok(())
    }

    /// Sends the steps the app hasn't had yet, and any divergence it
    /// hasn't been told of.
    async fn news(&mut self) -> Result<(), Gone> {
        self.steps().await?;
        self.diverged().await
    }

    /// Sends the steps the app hasn't had yet.
    async fn steps(&mut self) -> Result<(), Gone> {
        let new = self.book.after(self.sent);
        for steps in batches(&new, STEPS_AT_ONCE, STEPS_BYTES) {
            let Some(first) = steps.first() else { break };
            let message = Steps::Steps {
                from: first.no,
                steps,
            };
            self.send(&message).await?;
            self.sent = first.no + steps.len() as u64 - 1;
        }
        Ok(())
    }

    /// Sends each divergence the app hasn't been told of.
    async fn diverged(&mut self) -> Result<(), Gone> {
        for diverged in self.book.diverged() {
            if !self.told.contains(&diverged.check) {
                self.told.push(diverged.check);
                self.send(&diverged.message()).await?;
            }
        }
        Ok(())
    }

    /// Answers what the app asked for: the last move, then every state.
    async fn answer(&mut self, asked: Vec<FromInspector>) -> Result<(), Gone> {
        if asked.is_empty() {
            return Ok(());
        }
        // Steps the app hasn't had yet go first, so a position never
        // names a step it doesn't know.
        self.steps().await?;
        let end = self.sent;
        let moved =
            (asked.iter()).rfind(|message| !matches!(message, FromInspector::StateAt { .. }));
        let states: Vec<u64> = (asked.iter())
            .filter_map(|message| match message {
                FromInspector::StateAt { step } => Some((*step).min(end)),
                _ => None,
            })
            .collect();
        match moved {
            Some(FromInspector::Seek { step }) => {
                self.following = false;
                let at = (*step).min(end);
                self.position(at).await?;
                self.past(at).await?;
            }
            Some(FromInspector::Follow) => {
                if self.book.mode == Mode::Live {
                    self.following = true;
                    // The app's copies are `/viewer`'s now: the next seek
                    // sends each chat whole.
                    if let Some(player) = &mut self.player {
                        player.forget();
                    }
                }
                let at = if self.following {
                    end
                } else {
                    self.player.as_ref().map_or(0, Player::at)
                };
                self.position(at).await?;
            }
            _ => {}
        }
        // A step asked for again straight after is worked out once.
        let mut last: Option<(u64, ToInspector)> = None;
        for at in states {
            if !matches!(&last, Some((was, _)) if *was == at) {
                let core = blocking(&mut self.state, move |player| {
                    player.go(at);
                    player.state()
                })
                .await?;
                last = Some((at, ToInspector::State { at, core }));
            }
            if let Some((_, state)) = &last {
                self.send(state).await?;
            }
        }
        Ok(())
    }

    /// Tells the app where it's looking.
    async fn position(&mut self, at: u64) -> Result<(), Gone> {
        let position = ToInspector::Position {
            live: self.following,
            at,
            end: self.sent,
        };
        self.send(&position).await
    }

    /// Moves the player to `at`, and sends the chats as they were there.
    async fn past(&mut self, at: u64) -> Result<(), Gone> {
        let past = blocking(&mut self.player, move |player| {
            player.go(at);
            player.past()
        })
        .await?;
        for message in past {
            self.send(&ToInspector::Past { at, message }).await?;
        }
        Ok(())
    }

    async fn send(&mut self, message: &impl serde::Serialize) -> Result<(), Gone> {
        let text = serde_json::to_string(message).expect("inspector messages serialise");
        self.socket
            .send(Message::Text(text.into()))
            .await
            .map_err(|_| Gone)
    }
}

/// Does `work` with a player on a thread for blocking work, and puts it
/// back.
async fn blocking<T: Send + 'static>(
    player: &mut Option<Player>,
    work: impl FnOnce(&mut Player) -> T + Send + 'static,
) -> Result<T, Gone> {
    let Some(mut taken) = player.take() else {
        return Err(Gone);
    };
    let (taken, out) = tokio::task::spawn_blocking(move || {
        let out = work(&mut taken);
        (taken, out)
    })
    .await
    .map_err(|_| Gone)?;
    *player = Some(taken);
    Ok(out)
}

/// Steps cut into messages of at most `count` steps and, but for a single
/// step bigger than that, `bytes` of JSON.
fn batches(steps: &[Arc<Step>], count: usize, bytes: usize) -> Vec<&[Arc<Step>]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut size = 0;
    for (i, step) in steps.iter().enumerate() {
        let this = step.event.get().len() + step.directives.get().len();
        if i > start && (i - start == count || size + this > bytes) {
            out.push(&steps[start..i]);
            start = i;
            size = 0;
        }
        size += this;
    }
    if start < steps.len() {
        out.push(&steps[start..]);
    }
    out
}

/// [`ToInspector::Steps`], borrowing the book's steps: the same JSON.
#[derive(serde::Serialize)]
#[serde(tag = "type", content = "value")]
enum Steps<'a> {
    Steps { from: u64, steps: &'a [Arc<Step>] },
}

/// `/recording`: the live run's recording, as far as it's written, up to
/// its last whole line, read from the file as it's sent.
async fn recording(
    State(app): State<Arc<App>>,
    Query(token): Query<Token>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = allowed(&app, &token, &headers, true) {
        return refused;
    }
    let Some(path) = app.recording.clone() else {
        return (StatusCode::NOT_FOUND, "nothing is being recorded\n").into_response();
    };
    let (body, length) = match whole_lines(&path).await {
        Ok(whole) => whole,
        Err(error) => {
            log::warn!("couldn't read the recording {}: {error}", path.display());
            return (StatusCode::NOT_FOUND, "the recording couldn't be read\n").into_response();
        }
    };
    let name = (path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("recording.jsonl");
    let disposition = format!("attachment; filename=\"caf-{name}\"");
    (
        [
            (header::CONTENT_TYPE, "application/jsonl".to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, length.to_string()),
        ],
        body,
    )
        .into_response()
}

/// How much is read at once.
const CHUNK: usize = 64 << 10;

/// A file up to its last newline, as a body read as it's sent, and its
/// length: CAF writes a recording as it runs, so its end may be half a
/// line.
async fn whole_lines(path: &Path) -> std::io::Result<(Body, u64)> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut end = file.metadata().await?.len();
    // Back from the end, a chunk at a time, to the last newline.
    let mut chunk = vec![0; CHUNK];
    let whole = loop {
        if end == 0 {
            break 0;
        }
        let start = end.saturating_sub(CHUNK as u64);
        let len = usize::try_from(end - start).unwrap_or(CHUNK);
        file.seek(std::io::SeekFrom::Start(start)).await?;
        file.read_exact(&mut chunk[..len]).await?;
        if let Some(at) = chunk[..len].iter().rposition(|&byte| byte == b'\n') {
            break start + at as u64 + 1;
        }
        end = start;
    };
    file.seek(std::io::SeekFrom::Start(0)).await?;
    let stream = futures_util::stream::try_unfold((file, whole), |(mut file, left)| async move {
        if left == 0 {
            return Ok(None);
        }
        let mut chunk = vec![0; usize::try_from(left).unwrap_or(CHUNK).min(CHUNK)];
        let read = file.read(&mut chunk).await?;
        if read == 0 {
            return Ok::<_, std::io::Error>(None);
        }
        chunk.truncate(read);
        Ok(Some((Bytes::from(chunk), (file, left - read as u64))))
    });
    Ok((Body::from_stream(stream), whole))
}

/// What the app's page may load and do: only its own scripts, styles,
/// images and sockets, and no page may frame it. The agent's words are
/// drawn as Markdown with HTML off, but should anything slip through, it
/// can't load or run anything from elsewhere, or send the token there.
/// Images may also be `data:` URIs, which load nothing from anywhere: a
/// tool call's pictures are drawn as those.
pub const CSP: &str =
    "default-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'";

/// Everything else: a file of the built app, under [`CSP`].
async fn static_file(State(app): State<Arc<App>>, uri: Uri) -> Response {
    let mut response = app_file(&app, &uri).await;
    (response.headers_mut()).insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    response
}

/// A file of the built app, or why not.
async fn app_file(app: &App, uri: &Uri) -> Response {
    let index = app.dist.join("index.html");
    if !index.is_file() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            NOT_BUILT,
        )
            .into_response();
    }
    let path = uri.path().trim_start_matches('/');
    let file = if path.is_empty() {
        Some(index)
    } else {
        inside(&app.dist, path)
    };
    let Some(file) = file else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(&file).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type(&file))], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A file under `dir` by a URL's path, if the path names one plainly:
/// no `..`, no empty or `.` parts.
fn inside(dir: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path);
    let plain = (relative.components()).all(|part| matches!(part, Component::Normal(_)))
        && !path.split('/').any(|part| part.is_empty() || part == ".");
    plain.then(|| dir.join(relative))
}

/// A file's type, by its extension.
fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The page shown while the app isn't built.
const NOT_BUILT: &str = "<!doctype html>
<meta charset=\"utf-8\">
<title>caf-debugger: the app isn't built</title>
<h1>The debugger's app isn't built</h1>
<p>The server is running, but there's no app to show: it serves
<code>ts/debugger/dist</code> from CAF's repository, which isn't there.
Build it, then reload this page:</p>
<pre>cd ts
npm ci
npm run build --workspace debugger</pre>
";

#[cfg(test)]
mod tests {
    use std::path::Path;

    use std::sync::Arc;

    use caf_core::time::{Mono, Now, Wall};

    use super::{batches, inside};
    use crate::inspector::Step;

    /// A step whose event is `size` bytes of JSON.
    fn step(no: u64, size: usize) -> Arc<Step> {
        let text = format!("\"{}\"", "x".repeat(size - 2));
        Arc::new(Step {
            no,
            now: Now {
                wall: Wall(0),
                mono: Mono(0),
            },
            event: serde_json::value::RawValue::from_string(text).unwrap(),
            directives: serde_json::value::RawValue::from_string("[]".into()).unwrap(),
        })
    }

    #[test]
    fn steps_are_sent_in_messages_of_a_most_count_and_size() {
        let lens = |steps: &[Arc<Step>], count, bytes| -> Vec<usize> {
            batches(steps, count, bytes)
                .iter()
                .map(|batch| batch.len())
                .collect()
        };
        let small: Vec<_> = (1..=7).map(|no| step(no, 10)).collect();
        assert_eq!(lens(&small, 3, 1000), [3, 3, 1]);
        // 12 bytes each, with the directives: three fit in 36.
        assert_eq!(lens(&small, 100, 36), [3, 3, 1]);
        // A step bigger than the most goes alone.
        let mixed = [step(1, 10), step(2, 100), step(3, 10)];
        assert_eq!(lens(&mixed, 100, 50), [1, 1, 1]);
        assert!(batches(&[], 100, 50).is_empty());
    }

    #[test]
    fn only_plain_paths_name_files_of_the_app() {
        let dir = Path::new("/dist");
        assert_eq!(
            inside(dir, "assets/app.js"),
            Some(dir.join("assets/app.js"))
        );
        assert_eq!(inside(dir, "../secret"), None);
        assert_eq!(inside(dir, "assets/../../secret"), None);
        assert_eq!(inside(dir, "/etc/passwd"), None);
        assert_eq!(inside(dir, "assets//app.js"), None);
        assert_eq!(inside(dir, "./app.js"), None);
    }
}
