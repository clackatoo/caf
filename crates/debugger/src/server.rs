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
//! - **The Origin** must be the server's own, `http://127.0.0.1:PORT` or
//!   `http://localhost:PORT`: always on a WebSocket, which a browser
//!   always sends it with. `/recording` may also have none, as a
//!   download link or a page's own `fetch` sends none.
//! - **No CORS:** nothing here says another origin may read it.
//! - **What the app sends is capped:** [`VIEWER_MESSAGE`] on `/viewer`,
//!   [`INSPECTOR_MESSAGE`] on `/inspect`. A bigger one ends the socket.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use caf_core::viewer::FromViewer;
use caf_driver_tokio::Caf;
use caf_driver_tokio::viewer::{ViewerRules, serve};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;

use crate::inspector::{FromInspector, Mode, Step, ToInspector};
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
    /// This launch's token.
    pub token: String,
    /// The origins that count as the server's own.
    pub origins: [String; 2],
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
            token,
            origins: [
                format!("http://127.0.0.1:{port}"),
                format!("http://localhost:{port}"),
            ],
            dist: dist(),
        }
    }
}

/// Where the built app is: `ts/debugger/dist` in CAF's repository.
#[must_use]
pub fn dist() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ts/debugger/dist")
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
    upgrade
        .max_message_size(INSPECTOR_MESSAGE)
        .max_frame_size(INSPECTOR_MESSAGE)
        .on_upgrade(move |socket| Inspector::new(socket, book).run())
}

/// One app's inspector channel.
struct Inspector {
    socket: WebSocket,
    book: Arc<Book>,
    /// How many steps it has been sent.
    sent: u64,
    /// Following live.
    following: bool,
    /// It has been told where the replay check failed.
    told_diverged: bool,
    /// The core at the position the app looks at.
    player: Player,
    /// The core whose state it last asked for.
    state: Player,
}

/// The socket has gone.
struct Gone;

impl Inspector {
    fn new(socket: WebSocket, book: Arc<Book>) -> Self {
        Self {
            socket,
            player: Player::new(Arc::clone(&book.base)),
            state: Player::new(Arc::clone(&book.base)),
            following: book.mode == Mode::Live,
            book,
            sent: 0,
            told_diverged: false,
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
                incoming = self.socket.recv() => match read::<FromInspector>(incoming) {
                    Read::Message(message) => {
                        if self.answer(message).await.is_err() {
                            return;
                        }
                    }
                    Read::Skip => {}
                    Read::Closed => return,
                    Read::Bad(code, why) => {
                        close(self.socket, code, why).await;
                        return;
                    }
                },
            }
        }
    }

    /// What the app is sent as the link starts.
    async fn start(&mut self) -> Result<(), Gone> {
        let hello = ToInspector::Hello {
            mode: self.book.mode,
            file: self.book.file.clone(),
            run: self.book.run.clone(),
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

    /// Sends the steps the app hasn't had yet, and the divergence if it's
    /// new.
    async fn news(&mut self) -> Result<(), Gone> {
        self.steps().await?;
        self.diverged().await
    }

    /// Sends the steps the app hasn't had yet.
    async fn steps(&mut self) -> Result<(), Gone> {
        let new = self.book.after(self.sent);
        for steps in new.chunks(STEPS_AT_ONCE) {
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

    /// Sends the divergence, if there is one and the app hasn't had it.
    async fn diverged(&mut self) -> Result<(), Gone> {
        if self.told_diverged {
            return Ok(());
        }
        if let Some(diverged) = self.book.diverged() {
            self.told_diverged = true;
            self.send(&diverged.message()).await?;
        }
        Ok(())
    }

    /// Answers one of the app's messages.
    async fn answer(&mut self, message: FromInspector) -> Result<(), Gone> {
        // Steps the app hasn't had yet go first, so a position never
        // names a step it doesn't know.
        self.steps().await?;
        let end = self.sent;
        match message {
            FromInspector::Seek { step } => {
                self.following = false;
                let at = step.min(end);
                self.position(at).await?;
                self.past(at).await
            }
            FromInspector::Follow => {
                if self.book.mode == Mode::Live {
                    self.following = true;
                    // The app's copies are `/viewer`'s now: the next seek
                    // sends each chat whole.
                    self.player.forget();
                }
                let at = if self.following {
                    end
                } else {
                    self.player.at()
                };
                self.position(at).await
            }
            FromInspector::StateAt { step } => {
                let at = step.min(end);
                let steps = self.book.after(0);
                self.state.go(at, &steps);
                let state = ToInspector::State {
                    at,
                    core: self.state.state(),
                };
                self.send(&state).await
            }
        }
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
        let steps = self.book.after(0);
        self.player.go(at, &steps);
        for message in self.player.past() {
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

/// [`ToInspector::Steps`], borrowing the book's steps: the same JSON.
#[derive(serde::Serialize)]
#[serde(tag = "type", content = "value")]
enum Steps<'a> {
    Steps { from: u64, steps: &'a [Arc<Step>] },
}

/// `/recording`: the live run's recording, as far as it's written.
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
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => crate::files::whole_lines(bytes),
        Err(error) => {
            log::warn!("couldn't read the recording {}: {error}", path.display());
            return (StatusCode::NOT_FOUND, "the recording couldn't be read\n").into_response();
        }
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("recording.jsonl");
    let disposition = format!("attachment; filename=\"caf-{name}\"");
    (
        [
            (header::CONTENT_TYPE, "application/jsonl".to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    )
        .into_response()
}

/// Everything else: a file of the built app.
async fn static_file(State(app): State<Arc<App>>, uri: Uri) -> Response {
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

    use super::inside;

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
