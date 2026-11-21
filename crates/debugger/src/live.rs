//! Running CAF live: its steps go into the book, each checked against two
//! replays, of the steps' own events and of the recording's lines
//! ([`Check`]).

use std::path::PathBuf;
use std::sync::Arc;

use caf_core::agent::AgentSpec;
use caf_core::host::Grouping;
use caf_core::time::Now;
use caf_core::{Core, Host, NoAppItems, OpenChat};
use caf_driver_tokio::viewer::ViewerRules;
use caf_driver_tokio::{Caf, Config, Running, Step as CafStep, Steps};
use serde_json::value::RawValue;
use tokio::sync::mpsc;

use crate::inspector::{Check, Mode, Step};
use crate::replay::{Base, take, to_json};
use crate::steps::{Book, Diverged};

/// CAF, running, with its steps going into a book.
pub struct Live {
    /// CAF.
    pub caf: Caf,
    /// CAF's task: done once it has shut down.
    pub running: Running,
    /// Its steps.
    pub book: Arc<Book>,
    /// What viewers may do.
    pub rules: Arc<ViewerRules>,
    /// The recording, if CAF could make it.
    pub recording: Option<PathBuf>,
}

/// The host the debugger is to CAF: it shows permission questions and
/// thinking, but no forms or links, and starts an adapter for each chat,
/// so chats with the fake agent, whose script serves one session, don't
/// share one.
#[must_use]
pub fn host() -> Host {
    let mut host = Host::new("caf-debugger", env!("CARGO_PKG_VERSION"));
    host.shows.permission_questions = true;
    host.shows.thinking = true;
    host.shows.forms = false;
    host.shows.links = false;
    host.grouping = Grouping::PerChat;
    host
}

/// What viewers may do: open a chat with any of `agents` in any absolute
/// folder, with no MCP servers, and act on every chat.
#[must_use]
pub fn rules(agents: &[AgentSpec]) -> ViewerRules {
    let names: Vec<_> = agents.iter().map(|agent| agent.name.clone()).collect();
    let mut rules = ViewerRules::default();
    rules.open = Box::new(move |request| {
        if !names.contains(&request.agent) || !request.cwd.is_absolute() {
            return None;
        }
        let mut open = OpenChat::new(request.agent.0.clone(), request.cwd.clone());
        open.session = request.session.clone();
        Some(open)
    });
    rules
}

/// Starts CAF with `agents`, recording to `record` if given, and checking
/// every step against replays of it. Must be called on a tokio runtime.
#[must_use]
pub fn start(agents: Vec<AgentSpec>, record: Option<PathBuf>) -> Live {
    let base = Base {
        host: host(),
        agents: agents.clone(),
    };
    let rules = Arc::new(rules(&agents));
    let mut config = Config::new(base.host.clone(), agents);
    config.record.clone_from(&record);
    let (tap, steps) = mpsc::unbounded_channel();
    let (caf, running) = Caf::start_watching(config, Tap(tap));
    // The kit makes the file as CAF starts, or says why it couldn't. The
    // name was free, and has the process's id in it, so a file there now
    // is this run's.
    let recording = record.filter(|path| {
        std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
    });
    let file = recording.as_ref().map(|path| path.display().to_string());
    let run = Some(caf.run().to_owned());
    let book = Arc::new(Book::new(Mode::Live, file, run, base));
    let checking = Arc::clone(&book);
    // On a thread of its own, so checking never slows CAF's tasks.
    std::thread::spawn(move || check(&checking, steps));
    Live {
        caf,
        running,
        book,
        rules,
        recording,
    }
}

/// A step, as CAF's task hands it on.
struct Raw {
    now: Now,
    event: Box<RawValue>,
    directives: Box<RawValue>,
    /// Its line in the recording, if it went into one.
    line: Option<String>,
}

/// Takes each step on CAF's task: it writes the event and directives as
/// JSON, which is all the time it takes there, and hands them on.
struct Tap(mpsc::UnboundedSender<Raw>);

impl Steps<NoAppItems> for Tap {
    fn step(&mut self, step: CafStep<'_, NoAppItems>) {
        // An event that won't write is left as `null`, which won't read
        // back, so the check reports it.
        let event = serde_json::value::to_raw_value(step.event).unwrap_or_else(|error| {
            log::error!("a step's event didn't write as JSON: {error}");
            RawValue::NULL.to_owned()
        });
        let raw = Raw {
            now: step.now,
            event,
            directives: to_json(step.directives),
            line: step.line.map(str::to_owned),
        };
        // Once the checker has gone, so has the debugger.
        let _ = self.0.send(raw);
    }
}

/// Checks every step, adds it to the book, and keeps the book's copies of
/// the core, until CAF's task ends.
///
/// Two cores replay CAF's events, and each must give out what CAF's own
/// gave out ([`Check`]):
/// - one is given the steps' own events. Its copies are the book's
///   checkpoints, so after it diverges it carries on, unchecked;
/// - one is given the recording's lines, while CAF records. A step with
///   no line means the recording has stopped, and so does this check.
fn check(book: &Book, mut steps: mpsc::UnboundedReceiver<Raw>) {
    let mut replay = book.base.core();
    let mut checking = true;
    // Made as the first step comes, if it has a line. Its agents are as
    // given: a recording's header leaves out their arguments and the
    // values of their variables, which only `Start`'s command holds, so a
    // core made from the header differs from this one there alone.
    let mut recording: Option<Core> = None;
    let mut first = true;
    let mut no = 0;
    while let Some(raw) = steps.blocking_recv() {
        no += 1;
        let diverged = check_steps(&mut replay, no, &raw);
        if first && raw.line.is_some() {
            recording = Some(book.base.core());
        }
        first = false;
        let mut recording_diverged = None;
        if let Some(core) = &mut recording {
            match &raw.line {
                Some(line) => {
                    recording_diverged = check_recording(core, no, &raw, line);
                    book.recording_checked_one();
                }
                None => {
                    log::info!("the recording has stopped, and so has the check of it");
                    recording = None;
                }
            }
        }
        book.checkpoint(no, &replay);
        book.add([Step {
            no,
            now: raw.now,
            event: raw.event,
            directives: raw.directives,
        }]);
        if let Some(diverged) = diverged.filter(|_| checking) {
            book.diverged_at(diverged);
            checking = false;
        }
        if let Some(diverged) = recording_diverged {
            book.diverged_at(diverged);
            recording = None;
        }
    }
    book.complete();
}

/// Gives the steps' replay core a step, and says how it diverged, if it
/// did.
fn check_steps(core: &mut Core, no: u64, raw: &Raw) -> Option<Diverged> {
    let got = take(core, raw.now, &raw.event)
        .ok()
        .map(|out| to_json(&out));
    let same = got.as_ref().is_some_and(|got| {
        json(&raw.directives).is_some_and(|expected| json(got) == Some(expected))
    });
    (!same).then(|| Diverged {
        check: Check::Steps,
        at: no,
        expected: raw.directives.clone(),
        got,
    })
}

/// Gives the recording's replay core a step's line, and says how it
/// diverged, if it did: but for what the recording hides ([`hidden`]).
fn check_recording(core: &mut Core, no: u64, raw: &Raw, line: &str) -> Option<Diverged> {
    #[derive(serde::Deserialize)]
    struct Line {
        now: Now,
        event: Box<RawValue>,
    }
    let got = (serde_json::from_str::<Line>(line).ok())
        .and_then(|line| take(core, line.now, &line.event).ok())
        .map(|out| to_json(&out));
    let same = got.as_ref().is_some_and(|got| {
        let expected = json(&raw.directives).map(|mut all| {
            hidden(&mut all);
            all
        });
        let got = json(got).map(|mut all| {
            hidden(&mut all);
            all
        });
        expected.is_some() && expected == got
    });
    (!same).then(|| Diverged {
        check: Check::Recording,
        at: no,
        expected: raw.directives.clone(),
        got,
    })
}

/// Raw JSON, read.
fn json(raw: &RawValue) -> Option<serde_json::Value> {
    serde_json::from_str(raw.get()).ok()
}

/// Directives with what a recording hides taken out, so a replay of the
/// recording and the live run can be compared. A recording hides the
/// values of MCP servers' variables and headers in the host's event that
/// opens a chat, and those reach the agent in the `mcpServers` of the
/// message that opens or loads its session: so in each `Write` to an
/// agent whose message has `params.mcpServers`, every server's `env` and
/// `headers` values become `null`. Its `bytes` are compared as the JSON
/// they hold; every other `Write`'s bytes stay as they are.
///
/// Nothing else a recording hides changes a directive:
/// - Claude's sign-in status, written with no params, is dropped by the
///   core either way;
/// - agents' arguments and variables, left out of the header, reach
///   only `Start`, and the check's core is made with them as given.
fn hidden(directives: &mut serde_json::Value) {
    let Some(all) = directives.as_array_mut() else {
        return;
    };
    for directive in all {
        if directive["type"] != "Agent" || directive["value"]["type"] != "Write" {
            continue;
        }
        let bytes = &mut directive["value"]["value"]["bytes"];
        let Some(mut message) = (serde_json::from_value::<Vec<u8>>(bytes.clone()).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        else {
            continue;
        };
        let Some(servers) = message["params"]["mcpServers"].as_array_mut() else {
            continue;
        };
        for server in servers {
            for list in ["env", "headers"] {
                for pair in server[list].as_array_mut().into_iter().flatten() {
                    pair["value"] = serde_json::Value::Null;
                }
            }
        }
        *bytes = message;
    }
}

#[cfg(test)]
mod tests {
    use caf_core::agent::{AgentSpec, SpawnCommand};
    use caf_core::profiles::ProfileChoice;
    use caf_core::time::{Mono, Now, Wall};
    use caf_core::viewer::OpenRequest;
    use serde_json::json;
    use serde_json::value::RawValue;

    use super::{Raw, check_recording, check_steps, hidden, host, rules};
    use crate::inspector::Check;
    use crate::replay::{Base, take, to_json};

    fn agents() -> Vec<AgentSpec> {
        vec![AgentSpec::command(
            "fake",
            SpawnCommand::new("/bin/true"),
            ProfileChoice::Cautious,
        )]
    }

    fn raw(event: &serde_json::Value, directives: Box<RawValue>) -> Raw {
        Raw {
            now: Now {
                wall: Wall(1000),
                mono: Mono(1000),
            },
            event: serde_json::value::to_raw_value(event).unwrap(),
            directives,
            line: None,
        }
    }

    fn base() -> Base {
        Base {
            host: host(),
            agents: agents(),
        }
    }

    /// The host's event opening a chat in `cwd`.
    fn open(cwd: &str) -> serde_json::Value {
        json!({"type": "Host", "value": {"type": "OpenChat", "value": {
            "chat": 1, "open": {"agent": "fake", "cwd": cwd, "session": {"type": "New"}, "mcp_servers": []},
        }}})
    }

    /// What a core gives out for `event`, as CAF's would.
    fn out(event: &serde_json::Value) -> Box<RawValue> {
        let step = raw(event, to_json(&[]));
        to_json(&take(&mut base().core(), step.now, &step.event).expect("it reads"))
    }

    #[test]
    fn the_steps_check_finds_directives_that_differ_and_events_that_dont_read() {
        let open = open("/work");
        let out = out(&open);
        assert_ne!(out.get(), "[]");
        assert!(check_steps(&mut base().core(), 1, &raw(&open, out.clone())).is_none());

        let diverged =
            check_steps(&mut base().core(), 7, &raw(&open, to_json(&[]))).expect("it differs");
        assert_eq!((diverged.check, diverged.at), (Check::Steps, 7));
        assert_eq!(diverged.expected.get(), "[]");
        assert_eq!(
            diverged.got.map(|got| got.get().to_owned()),
            Some(out.get().to_owned())
        );

        let unreadable = raw(&json!({"type": "Nonsense"}), to_json(&[]));
        let diverged = check_steps(&mut base().core(), 2, &unreadable).expect("it differs");
        assert!(diverged.got.is_none());
    }

    #[test]
    fn the_recording_check_replays_the_line_and_finds_what_differs() {
        let live = open("/work");
        let raw = raw(&live, out(&live));
        let line = |event: &serde_json::Value| {
            json!({"now": {"wall": 1000, "mono": 1000}, "event": event}).to_string()
        };
        assert!(check_recording(&mut base().core(), 1, &raw, &line(&live)).is_none());
        // A line that replays otherwise: here, opening with an agent
        // there isn't, which the core refuses.
        let mut other = live.clone();
        other["value"]["value"]["open"]["agent"] = json!("nobody");
        let diverged =
            check_recording(&mut base().core(), 3, &raw, &line(&other)).expect("it differs");
        assert_eq!((diverged.check, diverged.at), (Check::Recording, 3));
        assert!(diverged.got.is_some());
        let diverged =
            check_recording(&mut base().core(), 3, &raw, "not a line").expect("it differs");
        assert!(diverged.got.is_none());
    }

    /// A `Write` of `message` to process 3.
    fn write(message: &serde_json::Value) -> serde_json::Value {
        let bytes = serde_json::to_vec(message).unwrap();
        json!({"type": "Agent", "value": {"type": "Write", "value": {"process": 3, "bytes": bytes}}})
    }

    #[test]
    fn only_mcp_servers_values_are_hidden_from_the_recording_check() {
        let session = |env: &str, header: &str, name: &str| {
            write(
                &json!({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"cwd": "/w", "mcpServers": [
                    {"name": name, "command": "/bin/gh", "args": [], "env": [{"name": "TOKEN", "value": env}]},
                    {"type": "http", "name": "maps", "url": "http://x", "headers": [{"name": "Authorization", "value": header}]},
                ]}}),
            )
        };
        let compared = |directive: serde_json::Value| {
            let mut all = json!([directive]);
            hidden(&mut all);
            all
        };
        // The values alone may differ.
        assert_eq!(
            compared(session("s3cret", "Bearer s3cret", "gh")),
            compared(session("[redacted]", "[redacted]", "gh"))
        );
        // Not a name, nor anything else.
        assert_ne!(
            compared(session("s3cret", "Bearer s3cret", "gh")),
            compared(session("[redacted]", "[redacted]", "github"))
        );
        let prompt = |text: &str| {
            write(
                &json!({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {"prompt": [{"type": "text", "text": text}]}}),
            )
        };
        assert_ne!(compared(prompt("a")), compared(prompt("b")));
        // Another write's bytes stay bytes, compared exactly.
        assert_eq!(compared(prompt("a")), json!([prompt("a")]));
    }

    #[test]
    fn viewers_may_open_the_agents_on_offer_in_absolute_folders_with_no_tools() {
        let rules = rules(&agents());
        let open = rules.open.as_ref();
        let mut request = OpenRequest::new("fake", "/home/me/project");
        let chat = open(&request).expect("allowed");
        assert_eq!(chat.agent.0, "fake");
        assert!(chat.mcp_servers.is_empty());
        request.cwd = "project".into();
        assert!(open(&request).is_none(), "a relative folder");
        assert!(
            open(&OpenRequest::new("claude", "/home/me")).is_none(),
            "not on offer"
        );
    }
}
