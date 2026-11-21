//! Sign-in, sign-out and status commands: the setup driver's work with
//! the agent's own programs.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use caf_core::agent::SpawnCommand;
use caf_core::event::SetupEvent;
use caf_core::install::{LinkFrom, SignedInAs};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot};

use super::install::{POLL, kill_group, path_var};
use super::{Reporter, hidden_name};
use crate::agent::command_for;

/// How long a command that isn't a sign-in may take.
const RUN: Duration = Duration::from_secs(120);

/// How long a status command may take.
const STATUS: Duration = Duration::from_secs(60);

/// The longest line of a sign-in's output looked at.
const LONGEST_LINE: usize = 64 << 10;

/// The most of a status command's answer read.
const LONGEST_ANSWER: u64 = 1 << 20;

/// The longest link or code reported.
const LONGEST_LINK: usize = 8 << 10;

/// A sign-in to run.
pub(super) struct SignIn {
    pub(super) command: SpawnCommand,
    pub(super) link: LinkFrom,
    pub(super) code_after: Option<String>,
    /// What the user pastes, if the recipe lets them: then, and only then,
    /// the command has a standard input.
    pub(super) pasted: Option<mpsc::UnboundedReceiver<String>>,
    /// Where its `BROWSER` folder is made.
    pub(super) temp: PathBuf,
}

/// Runs a sign-in command, reporting its link and code, until it ends or
/// is stopped.
pub(super) async fn sign_in(sign_in: SignIn, reporter: Reporter, mut stop: oneshot::Receiver<()>) {
    let SignIn {
        mut command,
        link,
        code_after,
        mut pasted,
        temp,
    } = sign_in;
    let marker = match &link {
        LinkFrom::Output(marker) => Some(marker.clone()),
        // `Browser`, and any kind added later, which has no marker.
        _ => None,
    };
    let browser = match link {
        LinkFrom::Browser => match Browser::make(&temp).await {
            Ok(browser) => {
                (command.env).push(caf_core::agent::EnvChange::Set(
                    "BROWSER".into(),
                    browser.script().into(),
                ));
                Some(browser)
            }
            Err(error) => {
                reporter.last(SetupEvent::Failed(format!(
                    "a browser for the sign-in couldn't be made: {error}"
                )));
                return;
            }
        },
        _ => None,
    };
    let mut process = command_for(&command, Some(path_var()));
    process
        .stdin(if pasted.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match tokio::process::Command::from(process)
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            reporter.last(SetupEvent::Failed(format!(
                "the sign-in couldn't be started: {error}"
            )));
            return;
        }
    };
    let mut stdin = child.stdin.take();
    // Each output is read on a task of its own, line by line, for the
    // link and code: what's found comes back here, and nothing else.
    let (found, mut finds) = mpsc::unbounded_channel();
    let readers = [
        child
            .stdout
            .take()
            .map(|out| watch_lines(out, marker.clone(), code_after.clone(), found.clone())),
        child
            .stderr
            .take()
            .map(|out| watch_lines(out, marker, code_after, found.clone())),
    ];
    drop(found);
    let mut seen = HashSet::new();
    let mut report = |event: SetupEvent| {
        let key = format!("{event:?}");
        if seen.insert(key) {
            reporter.send(event);
        }
    };
    let mut looks = tokio::time::interval(POLL);
    let ok = loop {
        tokio::select! {
            status = child.wait() => break status.is_ok_and(|status| status.success()),
            _ = &mut stop => {
                kill_group(&child);
                let _ = child.wait().await;
                break false;
            }
            Some(event) = finds.recv() => report(event),
            text = recv(pasted.as_mut()) => match text {
                // Written while Stop is still heard: a command that doesn't
                // read its input mustn't hold it up.
                Some(text) => {
                    let stopped = tokio::select! {
                        () = give(&mut stdin, &text) => false,
                        _ = &mut stop => true,
                    };
                    if stopped {
                        kill_group(&child);
                        let _ = child.wait().await;
                        break false;
                    }
                }
                None => pasted = None,
            },
            _ = looks.tick(), if browser.is_some() => {
                if let Some(browser) = &browser {
                    for link in browser.links() {
                        report(SetupEvent::Link(link));
                    }
                }
            }
        }
    };
    // What it said, or handed its browser, before it ended comes before
    // the end. Something it started may hold its output open, so that's
    // waited for only a moment, and then not read any more.
    let mut readers: Vec<_> = readers.into_iter().flatten().collect();
    let _ = tokio::time::timeout(POLL * 5, async {
        for reader in &mut readers {
            let _ = reader.await;
        }
    })
    .await;
    for reader in readers {
        reader.abort();
    }
    while let Ok(event) = finds.try_recv() {
        report(event);
    }
    if let Some(browser) = &browser {
        for link in browser.links() {
            report(SetupEvent::Link(link));
        }
    }
    drop(browser);
    reporter.last(SetupEvent::Ended { ok });
}

/// The next text pasted, or never if there's no way to paste.
async fn recv(pasted: Option<&mut mpsc::UnboundedReceiver<String>>) -> Option<String> {
    match pasted {
        Some(pasted) => pasted.recv().await,
        None => std::future::pending().await,
    }
}

/// Gives the sign-in what was pasted: its first line, trimmed, and a line
/// break.
async fn give(stdin: &mut Option<ChildStdin>, text: &str) {
    let line = text.lines().next().unwrap_or_default().trim();
    if let Some(input) = stdin {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        if input.write_all(&bytes).await.is_err() || input.flush().await.is_err() {
            *stdin = None;
        }
    }
}

/// Reads `output` line by line, sending the link after `marker` and the
/// code after `code_after` as they appear. Lines longer than
/// [`LONGEST_LINE`] are skipped.
fn watch_lines(
    output: impl AsyncRead + Unpin + Send + 'static,
    marker: Option<String>,
    code_after: Option<String>,
    found: mpsc::UnboundedSender<SetupEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut output = BufReader::new(output);
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = (&mut output)
                .take(LONGEST_LINE as u64 + 1)
                .read_until(b'\n', &mut line)
                .await;
            match read {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if line.len() > LONGEST_LINE && line.last() != Some(&b'\n') {
                // Too long: skipped to its end.
                if skip_line(&mut output).await.is_err() {
                    return;
                }
                continue;
            }
            let text = plain(&String::from_utf8_lossy(&line));
            if let Some(link) = marker.as_deref().and_then(|marker| after(&text, marker)) {
                let _ = found.send(SetupEvent::Link(link));
            }
            if let Some(code) = code_after
                .as_deref()
                .and_then(|before| after(&text, before))
            {
                let _ = found.send(SetupEvent::Code(code));
            }
        }
    })
}

/// Reads up to the end of the current line.
async fn skip_line(output: &mut (impl tokio::io::AsyncBufRead + Unpin)) -> io::Result<()> {
    loop {
        let buffer = output.fill_buf().await?;
        if buffer.is_empty() {
            return Ok(());
        }
        if let Some(end) = memchr::memchr(b'\n', buffer) {
            output.consume(end + 1);
            return Ok(());
        }
        let len = buffer.len();
        output.consume(len);
    }
}

/// What follows `before` in `line`, up to the next space, if anything does
/// and it isn't too long.
fn after(line: &str, before: &str) -> Option<String> {
    if before.is_empty() {
        return None;
    }
    let (_, rest) = line.split_once(before)?;
    let word = rest.split_whitespace().next()?;
    (word.len() <= LONGEST_LINK).then(|| word.to_owned())
}

/// `line` without terminal escape sequences or other control characters.
fn plain(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                // A control sequence: up to its final byte.
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // An operating system command, such as a hyperlink: up to
                // its end.
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\t' => plain.push(' '),
            c if c.is_control() => {}
            c => plain.push(c),
        }
    }
    plain
}

/// Runs a command, such as signing out, and says whether it succeeded.
/// Its output is thrown away unread.
pub(super) async fn run(
    command: SpawnCommand,
    reporter: Reporter,
    mut stop: oneshot::Receiver<()>,
) {
    let mut child = match start(&command, false) {
        Ok(child) => child,
        Err(error) => {
            reporter.last(SetupEvent::Failed(format!(
                "the command couldn't be started: {error}"
            )));
            return;
        }
    };
    let event = tokio::select! {
        status = child.wait() => SetupEvent::Ended { ok: status.is_ok_and(|status| status.success()) },
        _ = &mut stop => {
            kill_group(&child);
            let _ = child.wait().await;
            SetupEvent::Ended { ok: false }
        }
        () = tokio::time::sleep(RUN) => {
            kill_group(&child);
            let _ = child.wait().await;
            SetupEvent::Failed("the command took too long".into())
        }
    };
    reporter.last(event);
}

/// Runs a status command, and reports only the fields its recipe names.
pub(super) async fn status(
    command: SpawnCommand,
    signed_in: String,
    how: Option<(String, Vec<(String, SignedInAs)>)>,
    reporter: Reporter,
    mut stop: oneshot::Receiver<()>,
) {
    let mut child = match start(&command, true) {
        Ok(child) => child,
        Err(error) => {
            reporter.last(SetupEvent::Failed(format!(
                "the status command couldn't be started: {error}"
            )));
            return;
        }
    };
    let stdout = child.stdout.take();
    let answer = async {
        let mut answer = Vec::new();
        if let Some(stdout) = stdout {
            let _ = stdout.take(LONGEST_ANSWER).read_to_end(&mut answer).await;
            // Closed, so one with more to say than is read stops, rather
            // than waiting to be read.
        }
        let status = child.wait().await;
        (answer, status)
    };
    let (answer, status) = tokio::select! {
        done = answer => done,
        _ = &mut stop => {
            kill_group(&child);
            let _ = child.wait().await;
            reporter.last(SetupEvent::Ended { ok: false });
            return;
        }
        () = tokio::time::sleep(STATUS) => {
            kill_group(&child);
            let _ = child.wait().await;
            reporter.last(SetupEvent::Failed("the status command took too long".into()));
            return;
        }
    };
    let event = match read_status(&answer, &signed_in, how.as_ref()) {
        Some(status) => SetupEvent::Status(status),
        None => SetupEvent::Failed(match status {
            Ok(status) if !status.success() => match status.code() {
                Some(code) => format!("the status command exited with {code}, with no answer"),
                None => "the status command was killed".into(),
            },
            _ => "the status command's answer can't be read".into(),
        }),
    };
    reporter.last(event);
}

/// Reads a status command's JSON answer: only the fields at `signed_in`
/// and `how`. `None` if there's no `true` or `false` at `signed_in`.
fn read_status(
    answer: &[u8],
    signed_in: &str,
    how: Option<&(String, Vec<(String, SignedInAs)>)>,
) -> Option<Option<SignedInAs>> {
    let answer: serde_json::Value = serde_json::from_slice(answer).ok()?;
    if !answer.pointer(signed_in)?.as_bool()? {
        return Some(None);
    }
    let kind = how.map_or(SignedInAs::Unknown, |(pointer, kinds)| {
        let value = answer.pointer(pointer).and_then(serde_json::Value::as_str);
        (kinds.iter())
            .find(|(name, _)| Some(name.as_str()) == value)
            .map_or(SignedInAs::Unknown, |&(_, kind)| kind)
    });
    Some(Some(kind))
}

/// Starts `command` in a process group of its own, with no input, and its
/// output piped if `output`, or thrown away.
fn start(command: &SpawnCommand, output: bool) -> io::Result<Child> {
    let mut process = command_for(command, Some(path_var()));
    process
        .stdin(Stdio::null())
        .stdout(if output {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null());
    tokio::process::Command::from(process)
        .kill_on_drop(true)
        .spawn()
}

/// The `BROWSER` a sign-in is given: a script in a folder of its own,
/// readable only by the user, which writes each link it's given to a file
/// beside it. The folder goes when this is dropped.
struct Browser {
    folder: PathBuf,
}

/// The script. It writes its arguments, one a line, to a hidden file,
/// then renames it, so the driver never reads half a link.
const SCRIPT: &str = r#"#!/bin/sh
folder=$(dirname "$0")
printf '%s\n' "$@" > "$folder/.link$$" && mv "$folder/.link$$" "$folder/link$$"
"#;

impl Browser {
    /// Makes one, in a new folder in `temp`, and runs it once, so it's
    /// known to run: see [`Browser::runs`].
    async fn make(temp: &Path) -> io::Result<Self> {
        let browser = Self::write(temp)?;
        browser.runs().await?;
        Ok(browser)
    }

    /// Waits until the script can be run. Just written, it can't for a
    /// moment ("text file busy") while a process another thread started
    /// meanwhile holds it open for writing, until that process starts its
    /// own program. Once it has run, it always can: nothing else can open
    /// it for writing. Run with no link, it gives none.
    async fn runs(&self) -> io::Result<()> {
        let give_up = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let ran = tokio::process::Command::new(self.script())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
            match ran {
                Err(error)
                    if error.raw_os_error() == Some(libc::ETXTBSY)
                        && tokio::time::Instant::now() < give_up =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
                Ok(_) => return Ok(()),
            }
        }
    }

    /// Writes one, in a new folder in `temp`.
    fn write(temp: &Path) -> io::Result<Self> {
        let folder = temp.join(hidden_name("caf-browser", "sign-in").trim_start_matches('.'));
        fs::DirBuilder::new().mode(0o700).create(&folder)?;
        let browser = Self { folder };
        let mut script = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(browser.script())?;
        io::Write::write_all(&mut script, SCRIPT.as_bytes())?;
        Ok(browser)
    }

    fn script(&self) -> PathBuf {
        self.folder.join("browser")
    }

    /// The links handed to it since it was last looked at, each once: of
    /// each run's arguments, those that are links (with `://`), as they
    /// came, so an option such as `--new-window` before one is passed
    /// over.
    fn links(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.folder) else {
            return Vec::new();
        };
        let mut links = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_name().to_string_lossy().starts_with("link") {
                continue;
            }
            let given = read_small(&path);
            let _ = fs::remove_file(&path);
            // One argument a line: the links among them, not options such
            // as `--new-window` that some programs put first.
            for link in given.iter().flat_map(|given| given.lines()) {
                let link = link.trim();
                if link.contains("://")
                    && link.len() <= LONGEST_LINK
                    && !link.contains(char::is_control)
                {
                    links.push(link.to_owned());
                }
            }
        }
        links
    }
}

/// A file's text, if it's small.
fn read_small(path: &Path) -> Option<String> {
    let mut text = String::new();
    let file = fs::File::open(path).ok()?;
    io::Read::read_to_string(
        &mut io::Read::take(file, LONGEST_LINK as u64 + 1),
        &mut text,
    )
    .ok()?;
    Some(text)
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.folder);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_browser_hands_back_its_link_however_it_was_given() {
        let temp = std::env::temp_dir().join(format!("caf-browser-args-{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        let browser = Browser::make(&temp).await.unwrap();
        let link = "https://claude.ai/oauth/authorize?code=true&state=a%20b&scope=x+y#frag";
        let run = |args: &[&str]| {
            let status = std::process::Command::new(browser.script())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        run(&[link]);
        assert_eq!(browser.links(), [link]);
        // As some programs run a browser: with options before the link.
        run(&["--new-window", link]);
        assert_eq!(browser.links(), [link]);
        // Or several links at once: each is handed back.
        run(&[link, "https://claude.com/other"]);
        let mut links = browser.links();
        links.sort();
        assert_eq!(
            links,
            [
                "https://claude.ai/oauth/authorize?code=true&state=a%20b&scope=x+y#frag",
                "https://claude.com/other"
            ]
        );
        drop(browser);
        fs::remove_dir(&temp).unwrap();
    }

    #[tokio::test]
    async fn a_browser_is_ready_once_it_can_run() {
        let temp = std::env::temp_dir().join(format!("caf-browser-test-{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        let browser = Browser::write(&temp).unwrap();
        // Held open for writing, as a process another thread is starting
        // may hold it for a moment.
        let writing = fs::OpenOptions::new()
            .write(true)
            .open(browser.script())
            .unwrap();
        let let_go = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(writing);
        });
        browser.runs().await.expect("it runs once it can");
        let_go.await.unwrap();
        assert!(
            browser.links().is_empty(),
            "run with no link, it gives none"
        );
        drop(browser);
        assert!(
            fs::read_dir(&temp).unwrap().next().is_none(),
            "its folder goes"
        );
        fs::remove_dir(&temp).unwrap();
    }

    #[test]
    fn escape_sequences_and_controls_are_taken_out() {
        assert_eq!(
            plain("\u{1b}[1;34mOpen: https://claude.ai/x\u{1b}[0m\r\n"),
            "Open: https://claude.ai/x"
        );
        assert_eq!(
            plain("\u{1b}]8;;https://evil.example\u{7}Go\u{1b}]8;;\u{7} here"),
            "Go here"
        );
    }

    #[test]
    fn a_link_or_code_is_the_word_after_its_marker() {
        assert_eq!(
            after("Visit: https://claude.ai/oauth?x=1 to sign in", "Visit: "),
            Some("https://claude.ai/oauth?x=1".into())
        );
        assert_eq!(after("Visit:", "Visit:"), None);
        assert_eq!(after("nothing here", "Visit:"), None);
        assert_eq!(after("anything", ""), None);
    }

    #[test]
    fn only_the_named_fields_of_a_status_are_read() {
        let how = (
            "/authMethod".to_owned(),
            vec![
                ("claude.ai".to_owned(), SignedInAs::Account),
                ("console".to_owned(), SignedInAs::ApiKey),
            ],
        );
        let read = |json: &str| read_status(json.as_bytes(), "/loggedIn", Some(&how));
        assert_eq!(
            read(r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "me@example.com"}"#),
            Some(Some(SignedInAs::Account))
        );
        assert_eq!(
            read(r#"{"loggedIn": true, "authMethod": "console"}"#),
            Some(Some(SignedInAs::ApiKey))
        );
        assert_eq!(
            read(r#"{"loggedIn": true, "authMethod": "bedrock"}"#),
            Some(Some(SignedInAs::Unknown))
        );
        assert_eq!(
            read(r#"{"loggedIn": true}"#),
            Some(Some(SignedInAs::Unknown))
        );
        assert_eq!(read(r#"{"loggedIn": false}"#), Some(None));
        assert_eq!(read(r#"{"loggedIn": "yes"}"#), None);
        assert_eq!(read(r"{}"), None);
        assert_eq!(read("not json"), None);
        assert_eq!(
            read_status(br#"{"loggedIn": true}"#, "/loggedIn", None),
            Some(Some(SignedInAs::Unknown))
        );
    }
}
