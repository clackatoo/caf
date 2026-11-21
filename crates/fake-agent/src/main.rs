//! `caf-fake-agent SCRIPT [--record RECORD]`, or
//! `caf-fake-agent [--cli] auth login|status|logout`: see the library's
//! docs.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use caf_fake_agent::{EXIT_BAD_SCRIPT, RECORD_VAR, SCRIPT_VAR, Script, auth, run};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.as_slice() {
        // A process started by `spawn_holder`: it holds the pipes it was
        // given, then goes. Matched first, so a holder never runs a
        // script, whatever its environment says.
        [hold, secs, pid_file @ ..] if hold == "--hold" && pid_file.len() <= 1 => {
            hold_pipes(secs, pid_file.first().map(Path::new));
            0
        }
        // A sign-in, sign-out or status command, run as an adapter's own
        // command line: before the script, as it's run with the adapter's
        // environment, which names one.
        [cli, word, rest @ ..] if cli == "--cli" && word == "auth" => auth(rest),
        [word, rest @ ..] if word == "auth" => auth(rest),
        // Started by a driver, which may add arguments of its own.
        _ if std::env::var_os(SCRIPT_VAR).is_some() => {
            let script = std::env::var_os(SCRIPT_VAR).map(PathBuf::from);
            let record = std::env::var_os(RECORD_VAR).map(PathBuf::from);
            start(script.as_deref().expect("it's set"), record.as_deref())
        }
        [script] => start(Path::new(script), None),
        [script, flag, record] if flag == "--record" => {
            start(Path::new(script), Some(Path::new(record)))
        }
        _ => {
            eprintln!(
                "usage: caf-fake-agent SCRIPT [--record RECORD], or set {SCRIPT_VAR} and {RECORD_VAR}"
            );
            EXIT_BAD_SCRIPT
        }
    };
    std::process::exit(code);
}

fn start(script: &Path, record: Option<&Path>) -> i32 {
    match Script::load(script) {
        Ok(script) => run(&script, record),
        Err(err) => {
            eprintln!("caf-fake-agent: {err}");
            EXIT_BAD_SCRIPT
        }
    }
}

/// A process started by `spawn_holder`: it holds the pipes it was given
/// for `secs` seconds at most, or until its pid file goes. It writes the
/// file itself, renamed into place so it's never read half written, so
/// it knows the file was there.
fn hold_pipes(secs: &str, pid_file: Option<&Path>) {
    let secs = Duration::from_secs(secs.parse().unwrap_or(60));
    let until = Instant::now().checked_add(secs);
    if let Some(pid_file) = pid_file {
        let new = pid_file.with_extension("new");
        let written = std::fs::write(&new, std::process::id().to_string())
            .and_then(|()| std::fs::rename(&new, pid_file));
        if written.is_err() {
            return;
        }
    }
    while until.is_none_or(|until| Instant::now() < until) {
        if pid_file.is_some_and(|pid_file| !pid_file.exists()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
