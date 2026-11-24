//! The `caf-relay` program, run as an agent runs it.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use caf_relay::{CHAT_VAR, CHECK_ANSWER, CHECK_VAR, RELAY_VAR, TOKEN_VAR};

fn relay() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_caf-relay"));
    command
        .env_remove(CHECK_VAR)
        .env_remove(RELAY_VAR)
        .env_remove(CHAT_VAR)
        .env_remove(TOKEN_VAR);
    command
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("caf-relay-p{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_check_says_what_it_is_and_nothing_more() {
    let out = relay().env(CHECK_VAR, "1").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("{CHECK_ANSWER}\n")
    );
}

#[test]
fn run_on_its_own_it_fails_and_says_why_on_stderr() {
    let out = relay().output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr).unwrap().contains(RELAY_VAR));
}

#[test]
fn a_socket_it_cant_reach_is_told_on_stderr_only() {
    let dir = folder("gone");
    let out = relay()
        .env(RELAY_VAR, dir.join("gone.sock"))
        .env(CHAT_VAR, "c")
        .env(TOKEN_VAR, "t")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr).unwrap().contains("gone.sock"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_missing_token_is_told_on_stderr_only() {
    let dir = folder("no-token");
    let out = relay()
        .env(RELAY_VAR, dir.join("s.sock"))
        .env(CHAT_VAR, "c")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr).unwrap().contains(TOKEN_VAR));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn it_relays_both_ways_and_exits_when_caf_closes() {
    let dir = folder("relays");
    let path = dir.join("s.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let mut child = relay()
        .env(RELAY_VAR, &path)
        .env(CHAT_VAR, "0193")
        .env(TOKEN_VAR, "beef")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (caf, _) = listener.accept().unwrap();
    let mut from_relay = BufReader::new(caf.try_clone().unwrap());
    let mut line = String::new();
    from_relay.read_line(&mut line).unwrap();
    assert_eq!(
        line,
        "{\"caf_relay\":1,\"chat\":\"0193\",\"token\":\"beef\"}\n"
    );

    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"ping\"}\n")
        .unwrap();
    line.clear();
    from_relay.read_line(&mut line).unwrap();
    assert_eq!(line, "{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"ping\"}\n");

    (&caf)
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n")
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    line.clear();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line, "{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n");

    drop(from_relay);
    drop(caf);
    let status = child.wait().unwrap();
    assert!(status.success());
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "", "stdout carries only what CAF sent");
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert_eq!(err, "");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn it_exits_when_the_agent_closes_its_stdin() {
    let dir = folder("stdin");
    let path = dir.join("s.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let mut child = relay()
        .env(RELAY_VAR, &path)
        .env(CHAT_VAR, "0193")
        .env(TOKEN_VAR, "beef")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (_caf, _) = listener.accept().unwrap();
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    std::fs::remove_dir_all(dir).unwrap();
}
