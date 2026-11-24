//! CAF's MCP relay: how an agent reaches the tools CAF serves it.
//!
//! An agent is given CAF's tools as an MCP server it starts itself, over
//! stdio: a program it runs, which speaks MCP on its stdin and stdout.
//! That program is the relay. It connects to the CAF that started the
//! agent, over a unix socket, says which chat it serves and proves it
//! with a token, then passes bytes both ways, unread, until either side
//! closes. CAF itself answers the agent.
//!
//! The relay is usually the app's own program, run again: CAF names
//! [`std::env::current_exe`] as the server's command, with
//! [`RELAY_VAR`], [`CHAT_VAR`] and [`TOKEN_VAR`] in its environment. So
//! an app that serves CAF's tools calls [`relay_if_asked`] first thing in
//! its `main`, before anything else it does, such as starting a runtime
//! or reading its arguments:
//!
//! ```no_run
//! fn main() {
//!     caf_relay::relay_if_asked();
//!     // the app as usual
//! }
//! ```
//!
//! The `caf-relay` program is that and nothing else, for apps that name it
//! instead.
//!
//! CAF checks the hook is there as it starts, by running the relay with
//! [`CHECK_VAR`] set and expecting [`CHECK_ANSWER`] back: an app that
//! forgot it fails to start, rather than its agents silently going
//! without tools.
//!
//! The relay uses std only, and starts no runtime: it's run every time an
//! agent's session starts.

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

/// Set to `1`, the relay prints [`CHECK_ANSWER`] and exits 0.
pub const CHECK_VAR: &str = "CAF_RELAY_CHECK";
/// What the check prints, on a line of its own.
pub const CHECK_ANSWER: &str = "caf-relay 1";
/// The socket to relay to: set, the program is a relay.
pub const RELAY_VAR: &str = "CAF_RELAY";
/// The chat the relay serves, as CAF writes its id.
pub const CHAT_VAR: &str = "CAF_RELAY_CHAT";
/// The chat's token, which CAF checks before it serves it.
pub const TOKEN_VAR: &str = "CAF_RELAY_TOKEN";

/// Relays, if CAF started this program as a relay, and exits; returns at
/// once otherwise.
///
/// - With [`CHECK_VAR`] set to `1`, it prints [`CHECK_ANSWER`] and exits 0.
/// - With [`RELAY_VAR`] set, it connects to that socket and sends the
///   handshake, one line of JSON:
///   `{"caf_relay":1,"chat":"<CAF_RELAY_CHAT>","token":"<CAF_RELAY_TOKEN>"}`.
///   Then it copies its stdin to the socket and the socket to its stdout,
///   and exits as soon as either closes: 0 then, 1 if it couldn't connect.
///   Its stdout carries nothing but what the socket sent; what went wrong
///   goes to stderr.
///
/// Call it first thing in `main`: it never returns once it's relayed.
pub fn relay_if_asked() {
    if std::env::var_os(CHECK_VAR).is_some_and(|value| value == "1") {
        println!("{CHECK_ANSWER}");
        std::process::exit(0);
    }
    let Some(socket) = std::env::var_os(RELAY_VAR) else {
        return;
    };
    let code = match relay_stdio(Path::new(&socket)) {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("caf-relay: {why}");
            1
        }
    };
    std::process::exit(code);
}

fn relay_stdio(socket: &Path) -> Result<(), String> {
    let chat = var(CHAT_VAR)?;
    let token = var(TOKEN_VAR)?;
    // Unbuffered: what arrives is passed on at once, both ways.
    let stdin = std::fs::File::from(
        io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| format!("can't read stdin: {e}"))?,
    );
    let stdout = std::fs::File::from(
        io::stdout()
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| format!("can't write stdout: {e}"))?,
    );
    relay(socket, &chat, &token, stdin, stdout)
}

fn var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{RELAY_VAR} is set, but {name} isn't"))
}

/// Connects to `socket`, sends the handshake, and copies `input` to it and
/// it to `output` until either side closes.
fn relay(
    socket: &Path,
    chat: &str,
    token: &str,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
) -> Result<(), String> {
    let mut to_caf = UnixStream::connect(socket)
        .map_err(|e| format!("can't connect to {}: {e}", socket.display()))?;
    to_caf
        .write_all(handshake(chat, token).as_bytes())
        .map_err(|e| format!("can't send the handshake: {e}"))?;
    let from_caf = to_caf
        .try_clone()
        .map_err(|e| format!("can't share the socket: {e}"))?;
    let (done, ended) = mpsc::channel();
    let also_done = done.clone();
    // Whichever side closes first ends the relay: the thread still copying
    // the other way is left blocked, and goes when the process does.
    thread::spawn(move || {
        pump(input, to_caf, "to CAF");
        let _ = done.send(());
    });
    thread::spawn(move || {
        pump(from_caf, output, "to the agent");
        let _ = also_done.send(());
    });
    let _ = ended.recv();
    Ok(())
}

/// The first line a relay sends.
fn handshake(chat: &str, token: &str) -> String {
    format!(
        "{{\"caf_relay\":1,\"chat\":{},\"token\":{}}}\n",
        json_string(chat),
        json_string(token)
    )
}

/// `text` as a JSON string.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Copies `from` to `to` until `from` ends or either fails. A side that
/// closed is how a relay ends, so only other failures are told.
fn pump(mut from: impl Read, mut to: impl Write, way: &str) {
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return told(e, way),
        };
        if let Err(e) = to.write_all(&buf[..n]).and_then(|()| to.flush()) {
            return told(e, way);
        }
    }
}

fn told(e: io::Error, way: &str) {
    if !matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    ) {
        eprintln!("caf-relay: copying {way}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("caf-relay-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_handshake_is_one_line_of_json() {
        assert_eq!(
            handshake("0193-ab", "f00d"),
            "{\"caf_relay\":1,\"chat\":\"0193-ab\",\"token\":\"f00d\"}\n"
        );
    }

    #[test]
    fn whatever_is_in_the_handshake_stays_json() {
        assert_eq!(
            json_string("a\"b\\c\nd\u{1}é"),
            "\"a\\\"b\\\\c\\u000ad\\u0001é\""
        );
    }

    #[test]
    fn an_unreachable_socket_says_which() {
        let dir = folder("none");
        let socket = dir.join("gone.sock");
        let why = relay(&socket, "c", "t", io::empty(), io::sink()).unwrap_err();
        assert!(why.contains(&socket.display().to_string()), "{why}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The relay's stdin and stdout, as the agent's ends of two pipes.
    fn agent_ends() -> ((UnixStream, UnixStream), (UnixStream, UnixStream)) {
        (UnixStream::pair().unwrap(), UnixStream::pair().unwrap())
    }

    #[test]
    fn it_passes_bytes_both_ways_and_ends_when_caf_closes() {
        let dir = folder("caf-closes");
        let path = dir.join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let ((mut agent_in, relay_in), (relay_out, agent_out)) = agent_ends();
        let relaying = thread::spawn(move || relay(&path, "chat", "token", relay_in, relay_out));
        let (caf, _) = listener.accept().unwrap();
        let mut from_relay = io::BufReader::new(caf.try_clone().unwrap());
        let mut line = String::new();
        from_relay.read_line(&mut line).unwrap();
        assert_eq!(line, handshake("chat", "token"));
        agent_in.write_all(b"{\"id\":1}\n").unwrap();
        line.clear();
        from_relay.read_line(&mut line).unwrap();
        assert_eq!(line, "{\"id\":1}\n");
        (&caf).write_all(b"{\"id\":1,\"result\":{}}\n").unwrap();
        let mut to_agent = io::BufReader::new(agent_out);
        line.clear();
        to_agent.read_line(&mut line).unwrap();
        assert_eq!(line, "{\"id\":1,\"result\":{}}\n");
        drop(from_relay);
        drop(caf);
        assert_eq!(relaying.join().unwrap(), Ok(()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn it_ends_when_the_agent_closes_its_side() {
        let dir = folder("agent-closes");
        let path = dir.join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let ((agent_in, relay_in), (relay_out, _agent_out)) = agent_ends();
        let relaying = thread::spawn(move || relay(&path, "chat", "token", relay_in, relay_out));
        let (_caf, _) = listener.accept().unwrap();
        drop(agent_in);
        assert_eq!(relaying.join().unwrap(), Ok(()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
