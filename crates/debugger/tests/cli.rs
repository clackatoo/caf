//! The program itself: it shuts CAF down cleanly on any signal to stop.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Starts the debugger live, with the agents it offers by itself (the
/// fake agent, and Claude, installed by CAF), keeping its state and any
/// installs in a folder of its own; sends it `signal` the moment it says
/// it's serving, straight from this process, as fast as a script could;
/// and returns how it exited and the last event its recording holds.
fn stopped_by(signal: libc::c_int) -> (std::process::ExitStatus, serde_json::Value) {
    let state =
        std::env::temp_dir().join(format!("caf-debugger-cli-{signal}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_caf-debugger"))
        .args(["--port", "0"])
        .env("XDG_STATE_HOME", &state)
        .env("XDG_DATA_HOME", &state)
        .env(caf_fake_agent::BIN_VAR, caf_fake_agent::binary())
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut recording = None;
    for line in lines.by_ref() {
        let line = line.unwrap();
        if let Some(path) = line.strip_prefix("Recording to ") {
            recording = Some(PathBuf::from(path.trim_end_matches('.')));
        }
        if line.starts_with("Open http://127.0.0.1:") {
            break;
        }
    }
    let recording = recording.expect("it records");
    // Nothing reads what it prints any more, as when its terminal closes:
    // printing then fails, and must not stop it shutting down cleanly.
    drop(lines);
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: a plain syscall, to a child this test started and hasn't
    // reaped.
    assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "it stops");
        std::thread::sleep(Duration::from_millis(20));
    };
    let text = std::fs::read_to_string(&recording).unwrap();
    let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    std::fs::remove_dir_all(&state).unwrap();
    (status, last["event"].clone())
}

#[test]
fn sigterm_and_sighup_shut_caf_down_as_ctrl_c_does() {
    // Each a few times: the signal comes as it says it's serving, and
    // must be heard however soon that is.
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP].repeat(4) {
        let (status, last) = stopped_by(signal);
        assert!(status.success(), "{signal}: {status}");
        assert_eq!(
            last,
            serde_json::json!({"type": "Host", "value": {"type": "Shutdown"}}),
            "{signal}: the recording ends with CAF shutting down"
        );
    }
}
