//! The program itself: it shuts CAF down cleanly on any signal to stop.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Starts the debugger live, with the agents it offers by itself (the
/// fake agent, and Claude, installed by CAF), keeping its state and any
/// installs in a folder of its own; sends it `signal` the moment it says
/// it's serving, straight from this process, as fast as a script could;
/// and returns how it exited and the last event its recording holds but
/// the store's answers and the prices: listing the saved chats, and
/// reading the prices, as it starts may be answered after it's told to
/// stop. Its usage ledger is in the folder too, and fetches no prices.
fn stopped_by(signal: libc::c_int) -> (std::process::ExitStatus, serde_json::Value) {
    let state =
        std::env::temp_dir().join(format!("caf-debugger-cli-{signal}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_caf-debugger"))
        .args(["--port", "0", "--no-fetch-prices"])
        .env("XDG_STATE_HOME", &state)
        .env("XDG_DATA_HOME", &state)
        // No proxy: nothing here downloads, and nothing should try one.
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
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
    let events = (text.lines().skip(1))
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["event"].clone());
    let last = (events
        .filter(|event| !["Storage", "Prices"].contains(&event["type"].as_str().unwrap())))
    .last();
    std::fs::remove_dir_all(&state).unwrap();
    (status, last.unwrap_or_default())
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

#[test]
fn a_store_it_cant_write_in_stops_it_before_it_starts() {
    let state = std::env::temp_dir().join(format!("caf-debugger-cli-store-{}", std::process::id()));
    std::fs::create_dir_all(&state).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_caf-debugger"))
        .args([
            "--port",
            "0",
            "--no-fetch-prices",
            "--store",
            "/proc/caf-debugger-nowhere",
        ])
        .env("XDG_STATE_HOME", &state)
        .env("XDG_DATA_HOME", &state)
        .env(caf_fake_agent::BIN_VAR, caf_fake_agent::binary())
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    std::fs::remove_dir_all(&state).unwrap();
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{said}");
    assert!(
        said.contains("can't keep chats in /proc/caf-debugger-nowhere"),
        "{said}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Open http"));
}

#[test]
fn the_fake_saying_what_turns_spent_is_for_running_live() {
    let output = Command::new(env!("CARGO_BIN_EXE_caf-debugger"))
        .args(["--replay", "nowhere.jsonl", "--fake-usage"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        said.contains("--fake-usage is for running live, not replaying"),
        "{said}"
    );
}
