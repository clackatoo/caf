//! The agent driver, with real processes.

use std::path::Path;
use std::time::{Duration, Instant};

use caf_core::Ticket;
use caf_core::acp::{Frame, Message, RequestId, Shape};
use caf_core::agent::{EnvChange, SpawnCommand};
use caf_core::directive::{AgentDirective, Signal};
use caf_core::event::{AgentEvent, Exit};
use caf_driver_tokio::{AgentDriver, AgentLimits};

const ONE: Ticket = Ticket(1000);
const TWO: Ticket = Ticket(1001);

/// Long enough for anything a test waits for.
const PATIENCE: Duration = Duration::from_secs(30);

fn sh(script: &str) -> SpawnCommand {
    let mut command = SpawnCommand::new("/bin/sh");
    command.args = vec!["-c".into(), script.into()];
    command
}

fn start(driver: &mut AgentDriver, process: Ticket, command: SpawnCommand) {
    driver.apply(AgentDirective::Start { process, command });
}

fn write(driver: &mut AgentDriver, process: Ticket, bytes: impl Into<Vec<u8>>) {
    let bytes = bytes.into();
    driver.apply(AgentDirective::Write { process, bytes });
}

fn notification(method: &str) -> String {
    format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\"}}\n")
}

/// The method of a notification or request, if `event` is one.
fn method(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::Frame(Frame::Message(
            Message::Notification { method, .. } | Message::Request { method, .. },
        )) => Some(method),
        _ => None,
    }
}

/// What `process` reports, until `done` is true of what it has reported.
async fn until(
    driver: &mut AgentDriver,
    process: Ticket,
    mut done: impl FnMut(&[AgentEvent]) -> bool,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    let waited = tokio::time::timeout(PATIENCE, async {
        while !done(&events) {
            let (from, event) = driver.next().await;
            if from == process {
                events.push(event);
            }
        }
    })
    .await;
    assert!(waited.is_ok(), "gave up waiting, having seen {events:?}");
    events
}

/// What `process` reports until it has exited and its output has closed.
async fn until_done(driver: &mut AgentDriver, process: Ticket) -> Vec<AgentEvent> {
    until(driver, process, |events| {
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Exited(_)))
            && events
                .iter()
                .any(|event| matches!(event, AgentEvent::OutputClosed))
    })
    .await
}

/// Where in `events` the first that `is` is true of comes.
fn find(events: &[AgentEvent], is: impl Fn(&AgentEvent) -> bool) -> usize {
    events
        .iter()
        .position(is)
        .unwrap_or_else(|| panic!("not found in {events:?}"))
}

fn exit_of(events: &[AgentEvent]) -> Exit {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Exited(exit) => Some(*exit),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no exit in {events:?}"))
}

fn pid_of(events: &[AgentEvent]) -> u32 {
    match events.first() {
        Some(AgentEvent::Started { pid }) => *pid,
        _ => panic!("didn't start first: {events:?}"),
    }
}

fn stderr_of(events: &[AgentEvent]) -> Vec<u8> {
    (events.iter())
        .filter_map(|event| match event {
            AgentEvent::Stderr(bytes) => Some(&bytes[..]),
            _ => None,
        })
        .collect::<Vec<_>>()
        .concat()
}

/// Whether the process `pid` is running: there, and not a zombie.
fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, rest)| !rest.starts_with('Z'))
    })
}

/// Waits until `gone` is true, failing after a while.
async fn eventually(what: &str, mut gone: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !gone() {
        assert!(Instant::now() < deadline, "{what} never happened");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Asks whether `process`'s group is empty, until it is.
async fn until_group_empty(driver: &mut AgentDriver, process: Ticket) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        driver.apply(AgentDirective::CheckGroup { process });
        let events = until(driver, process, |events| {
            matches!(events.last(), Some(AgentEvent::Group { .. }))
        })
        .await;
        if matches!(events.last(), Some(AgentEvent::Group { empty: true })) {
            return;
        }
        assert!(Instant::now() < deadline, "the group never emptied");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The pid a script wrote first to its error output.
async fn pid_written(driver: &mut AgentDriver, process: Ticket) -> u32 {
    let events = until(driver, process, |events| stderr_of(events).contains(&b'\n')).await;
    let stderr = String::from_utf8(stderr_of(&events)).unwrap();
    stderr.lines().next().unwrap().trim().parse().unwrap()
}

#[tokio::test]
async fn a_process_starts_talks_and_exits_in_order() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(&mut driver, ONE, SpawnCommand::new("/bin/cat"));
    write(&mut driver, ONE, notification("a") + &notification("b"));
    driver.apply(AgentDirective::CloseInput { process: ONE });
    let events = until_done(&mut driver, ONE).await;
    let pid = pid_of(&events);
    assert!(pid > 0);
    let a = find(&events, |event| method(event) == Some("a"));
    let b = find(&events, |event| method(event) == Some("b"));
    let closed = find(&events, |event| matches!(event, AgentEvent::OutputClosed));
    // Whether what it wrote is read before its exit is told is best
    // effort; it's always read before its output closes.
    assert!(0 < a && a < b && b < closed, "{events:?}");
    assert_eq!(exit_of(&events), Exit::Code(0));
    assert_eq!(events.len(), 5, "{events:?}");
    // Reaped before its exit was told.
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test]
async fn frames_come_in_line_order_even_when_parsed_elsewhere() {
    let mut limits = AgentLimits::default();
    limits.parse_elsewhere = 1024;
    let mut driver = AgentDriver::new(limits);
    start(&mut driver, ONE, SpawnCommand::new("/bin/cat"));
    let count = 600;
    let mut input = String::new();
    for n in 0..count {
        // Every third line is big, and parsed elsewhere.
        let padding = if n % 3 == 0 {
            "x".repeat(8000 + n * 7)
        } else {
            String::new()
        };
        input += &format!(
            "{{\"jsonrpc\":\"2.0\",\"method\":\"m{n}\",\"params\":{{\"p\":\"{padding}\"}}}}\n"
        );
    }
    write(&mut driver, ONE, input);
    driver.apply(AgentDirective::CloseInput { process: ONE });
    let events = until_done(&mut driver, ONE).await;
    let methods: Vec<&str> = events.iter().filter_map(method).collect();
    let expected: Vec<String> = (0..count).map(|n| format!("m{n}")).collect();
    assert_eq!(methods, expected);
}

#[tokio::test]
async fn bad_utf8_is_handed_on_not_fatal() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    let script = r#"printf '\377\376 junk\n'; printf '{"jsonrpc":"2.0","method":"after"}\n'; printf 'bad \377 bytes\n' >&2; printf 'no newline'"#;
    start(&mut driver, ONE, sh(script));
    let events = until_done(&mut driver, ONE).await;
    let junk = find(&events, |event| {
        matches!(event, AgentEvent::Frame(Frame::Junk { len: 7 }))
    });
    let after = find(&events, |event| method(event) == Some("after"));
    // An unfinished last line is still handed on, before the output closes.
    let last = find(&events, |event| {
        matches!(event, AgentEvent::Frame(Frame::Junk { len: 10 }))
    });
    let closed = find(&events, |event| matches!(event, AgentEvent::OutputClosed));
    assert!(junk < after && after < last && last < closed, "{events:?}");
    assert_eq!(stderr_of(&events), b"bad \xff bytes\n");
    assert_eq!(exit_of(&events), Exit::Code(0));
}

#[tokio::test]
async fn a_line_too_big_is_dropped_and_its_id_kept() {
    let mut limits = AgentLimits::default();
    limits.biggest_line = 1024;
    let mut driver = AgentDriver::new(limits);
    start(&mut driver, ONE, SpawnCommand::new("/bin/cat"));
    let big = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"fs/write_text_file\",\"params\":\"{}\"}}\n",
        "x".repeat(5000)
    );
    write(&mut driver, ONE, big.clone() + &notification("next"));
    driver.apply(AgentDirective::CloseInput { process: ONE });
    let events = until_done(&mut driver, ONE).await;
    let too_big = find(&events, |event| {
        matches!(
            event,
            AgentEvent::Frame(Frame::TooBig { id: Some(RequestId::Number(5)), method: Some(method), shape: Shape::Call, len })
                if method == "fs/write_text_file" && *len == big.len() - 1
        )
    });
    assert!(too_big < find(&events, |event| method(event) == Some("next")));
}

#[tokio::test]
async fn a_flood_doesnt_starve_another_process() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(
        &mut driver,
        ONE,
        sh(r#"exec yes '{"jsonrpc":"2.0","method":"flood"}'"#),
    );
    start(
        &mut driver,
        TWO,
        sh(r#"sleep 0.2; printf '{"jsonrpc":"2.0","method":"hello"}\n'"#),
    );
    let mut floods = 0;
    let waited = tokio::time::timeout(PATIENCE, async {
        loop {
            match driver.next().await {
                (ONE, event) if method(&event) == Some("flood") => floods += 1,
                (TWO, event) if method(&event) == Some("hello") => break,
                _ => {}
            }
        }
    })
    .await;
    assert!(waited.is_ok(), "the flood starved the other process");
    assert!(floods > 1000, "it didn't flood: {floods}");
    driver.apply(AgentDirective::Signal {
        process: ONE,
        signal: Signal::Kill,
    });
    let events = until(&mut driver, ONE, |events| {
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Exited(_)))
    })
    .await;
    assert_eq!(exit_of(&events), Exit::Signal(9));
}

#[tokio::test]
async fn writing_never_blocks_reading() {
    // cat writes back what it reads: if writing its input stopped the
    // driver reading its output, both pipes would fill and neither side
    // would move again.
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(&mut driver, ONE, SpawnCommand::new("/bin/cat"));
    let line = format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":\"m\",\"params\":\"{}\"}}\n",
        "x".repeat(1000)
    );
    let count = 16 << 10;
    write(&mut driver, ONE, line.repeat(count));
    let mut seen = 0;
    until(&mut driver, ONE, |events| {
        seen = events
            .iter()
            .filter(|event| method(event) == Some("m"))
            .count();
        seen == count
    })
    .await;
    driver.apply(AgentDirective::CloseInput { process: ONE });
    let events = until_done(&mut driver, ONE).await;
    assert_eq!(exit_of(&events), Exit::Code(0));
}

#[tokio::test]
async fn closing_input_waits_for_what_is_unsent() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(&mut driver, ONE, SpawnCommand::new("/bin/cat"));
    // Far more than a pipe holds, so most is unsent when it's closed.
    let count = 4 << 10;
    let line = format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":\"m\",\"params\":\"{}\"}}\n",
        "x".repeat(500)
    );
    write(&mut driver, ONE, line.repeat(count));
    driver.apply(AgentDirective::CloseInput { process: ONE });
    let events = until_done(&mut driver, ONE).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| method(event) == Some("m"))
            .count(),
        count
    );
    assert_eq!(exit_of(&events), Exit::Code(0));
}

#[tokio::test]
async fn output_after_exit_is_still_read() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(
        &mut driver,
        ONE,
        sh(r#"(sleep 0.3; printf '{"jsonrpc":"2.0","method":"late"}\n') & exit 3"#),
    );
    let events = until_done(&mut driver, ONE).await;
    let exited = find(&events, |event| {
        matches!(event, AgentEvent::Exited(Exit::Code(3)))
    });
    let late = find(&events, |event| method(event) == Some("late"));
    let closed = find(&events, |event| matches!(event, AgentEvent::OutputClosed));
    assert!(exited < late && late < closed, "{events:?}");
}

#[tokio::test]
async fn a_grandchild_holding_the_pipes_is_waited_for_only_so_long() {
    let mut limits = AgentLimits::default();
    limits.drain = Duration::from_millis(300);
    let mut driver = AgentDriver::new(limits);
    start(&mut driver, ONE, sh("sleep 100 & echo $! >&2; exit 0"));
    let sleep = pid_written(&mut driver, ONE).await;
    let events = until(&mut driver, ONE, |events| {
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Exited(_)))
    })
    .await;
    assert_eq!(exit_of(&events), Exit::Code(0));
    let exited = Instant::now();
    until(&mut driver, ONE, |events| {
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::OutputClosed))
    })
    .await;
    let waited = exited.elapsed();
    assert!(waited < Duration::from_secs(5), "{waited:?}");

    // Its leader is reaped, but the group still has the sleep in it.
    assert!(running(sleep));
    driver.apply(AgentDirective::CheckGroup { process: ONE });
    let events = until(&mut driver, ONE, |events| !events.is_empty()).await;
    assert!(
        matches!(events[..], [AgentEvent::Group { empty: false }]),
        "{events:?}"
    );

    driver.apply(AgentDirective::Signal {
        process: ONE,
        signal: Signal::Kill,
    });
    until_group_empty(&mut driver, ONE).await;
    eventually("the sleep ending", || !running(sleep)).await;
    driver.apply(AgentDirective::Forget { process: ONE });
}

#[tokio::test]
async fn signals_reach_the_whole_group() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(&mut driver, ONE, sh("sleep 100 & echo $! >&2; wait"));
    let sleep = pid_written(&mut driver, ONE).await;
    assert!(running(sleep));
    driver.apply(AgentDirective::CheckGroup { process: ONE });
    let events = until(&mut driver, ONE, |events| {
        matches!(events.last(), Some(AgentEvent::Group { .. }))
    })
    .await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Group { empty: false })
    ));

    driver.apply(AgentDirective::Signal {
        process: ONE,
        signal: Signal::Term,
    });
    let events = until_done(&mut driver, ONE).await;
    assert_eq!(exit_of(&events), Exit::Signal(15));
    eventually("the sleep ending", || !running(sleep)).await;
    until_group_empty(&mut driver, ONE).await;
}

#[tokio::test]
async fn exits_are_told_by_code_or_signal() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(&mut driver, ONE, sh("exit 7"));
    assert_eq!(exit_of(&until_done(&mut driver, ONE).await), Exit::Code(7));
    start(&mut driver, TWO, sh("kill -9 $$"));
    assert_eq!(
        exit_of(&until_done(&mut driver, TWO).await),
        Exit::Signal(9)
    );
}

#[tokio::test]
async fn forgetting_a_running_process_kills_and_reaps_it() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(
        &mut driver,
        ONE,
        sh("sleep 100 & echo $! >&2; exec sleep 100"),
    );
    let sleep = pid_written(&mut driver, ONE).await;
    driver.apply(AgentDirective::CheckGroup { process: ONE });
    let mut leader = None;
    let events = until(&mut driver, ONE, |events| {
        matches!(events.last(), Some(AgentEvent::Group { .. }))
    })
    .await;
    if let Some(AgentEvent::Started { pid }) = events.first() {
        leader = Some(*pid);
    }
    driver.apply(AgentDirective::Forget { process: ONE });
    // Nothing more is told of it.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), driver.next())
            .await
            .is_err()
    );
    eventually("the sleeps ending", || !running(sleep)).await;
    if let Some(leader) = leader {
        eventually("the leader being reaped", || {
            !Path::new(&format!("/proc/{leader}")).exists()
        })
        .await;
    }
}

#[tokio::test]
async fn dropping_the_driver_kills_and_reaps_everything() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(&mut driver, ONE, sh("sleep 100 & echo $! >&2; wait"));
    let events = until(&mut driver, ONE, |events| {
        stderr_of(events).contains(&b'\n')
    })
    .await;
    let leader = pid_of(&events);
    let stderr = String::from_utf8(stderr_of(&events)).unwrap();
    let sleep: u32 = stderr.trim().parse().unwrap();
    drop(driver);
    eventually("the sleep ending", || !running(sleep)).await;
    eventually("the leader being reaped", || {
        !Path::new(&format!("/proc/{leader}")).exists()
    })
    .await;
}

#[tokio::test]
async fn a_program_that_cant_start_is_told() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(&mut driver, ONE, SpawnCommand::new("/no/such/program"));
    start(&mut driver, TWO, SpawnCommand::new("sh"));
    let one = until(&mut driver, ONE, |events| !events.is_empty()).await;
    assert!(
        matches!(&one[..], [AgentEvent::StartFailed { .. }]),
        "{one:?}"
    );
    // Never looked up on PATH.
    let two = until(&mut driver, TWO, |events| !events.is_empty()).await;
    assert!(
        matches!(&two[..], [AgentEvent::StartFailed { .. }]),
        "{two:?}"
    );
    // Directives for it do nothing.
    write(&mut driver, ONE, "x\n");
    driver.apply(AgentDirective::CheckGroup { process: ONE });
    assert!(
        tokio::time::timeout(Duration::from_millis(200), driver.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_write_error_drops_what_is_unsent_and_reading_goes_on() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    let script =
        r#"exec 0<&-; echo closed >&2; sleep 0.5; printf '{"jsonrpc":"2.0","method":"after"}\n'"#;
    start(&mut driver, ONE, sh(script));
    until(&mut driver, ONE, |events| stderr_of(events) == b"closed\n").await;
    // A process another test starts at the same moment can hold the pipe
    // open for a moment, as it starts, so a first write may still go
    // into the pipe. Writes go on until one fails.
    let mut events = Vec::new();
    while !events
        .iter()
        .any(|event| matches!(event, AgentEvent::WriteFailed { .. }))
    {
        write(&mut driver, ONE, notification("lost"));
        let waited = tokio::time::timeout(Duration::from_millis(10), driver.next()).await;
        if let Ok((ONE, event)) = waited {
            events.push(event);
        }
    }
    // Nothing more is written, so nothing more fails.
    write(&mut driver, ONE, notification("also lost"));
    events.extend(until_done(&mut driver, ONE).await);
    let failed = find(&events, |event| {
        matches!(event, AgentEvent::WriteFailed { .. })
    });
    let after = find(&events, |event| method(event) == Some("after"));
    assert!(failed < after, "{events:?}");
    let failures = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::WriteFailed { .. }));
    assert_eq!(failures.count(), 1, "{events:?}");
    assert_eq!(exit_of(&events), Exit::Code(0));
}

#[tokio::test]
async fn unsent_input_that_doesnt_move_is_a_write_failure() {
    let mut limits = AgentLimits::default();
    limits.unsent = 1 << 20;
    limits.stuck = Duration::from_millis(300);
    let mut driver = AgentDriver::new(limits);
    start(&mut driver, ONE, sh("exec sleep 100"));
    until(&mut driver, ONE, |events| !events.is_empty()).await;
    let wrote = Instant::now();
    write(&mut driver, ONE, vec![b'x'; 4 << 20]);
    until(&mut driver, ONE, |events| {
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::WriteFailed { .. }))
    })
    .await;
    assert!(wrote.elapsed() >= Duration::from_millis(250));
    driver.apply(AgentDirective::Signal {
        process: ONE,
        signal: Signal::Kill,
    });
    let events = until_done(&mut driver, ONE).await;
    assert_eq!(exit_of(&events), Exit::Signal(9));
}

#[tokio::test]
async fn one_write_bigger_than_the_cap_goes_to_a_process_that_reads() {
    let mut limits = AgentLimits::default();
    limits.unsent = 64 << 10;
    limits.stuck = Duration::from_millis(500);
    let mut driver = AgentDriver::new(limits);
    start(&mut driver, ONE, SpawnCommand::new("/bin/cat"));
    let count = 8 << 10;
    let line = format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":\"m\",\"params\":\"{}\"}}\n",
        "x".repeat(1000)
    );
    write(&mut driver, ONE, line.repeat(count));
    driver.apply(AgentDirective::CloseInput { process: ONE });
    let events = until_done(&mut driver, ONE).await;
    let failures = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::WriteFailed { .. }));
    assert_eq!(failures.count(), 0, "a healthy reader was failed");
    assert_eq!(
        events
            .iter()
            .filter(|event| method(event) == Some("m"))
            .count(),
        count
    );
}

#[tokio::test]
async fn the_last_output_is_kept_while_the_core_is_slow() {
    // Reading pauses at 1 KiB waiting, and the core takes nothing for far
    // longer than the drain: what the process wrote must all still come.
    let mut limits = AgentLimits::default();
    limits.parsing = 1 << 10;
    limits.drain = Duration::from_millis(200);
    let mut driver = AgentDriver::new(limits);
    let count = 1000;
    let script = format!(
        r#"i=0; while [ $i -lt {count} ]; do printf '{{"jsonrpc":"2.0","method":"m"}}\n'; i=$((i+1)); done"#
    );
    start(&mut driver, ONE, sh(&script));
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let events = until_done(&mut driver, ONE).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| method(event) == Some("m"))
            .count(),
        count
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_start_is_told_even_with_directives_right_behind_it() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    start(&mut driver, ONE, SpawnCommand::new("relative/program"));
    // Its task runs on another worker, and fails, before these come.
    std::thread::sleep(Duration::from_millis(100));
    write(&mut driver, ONE, notification("initialize"));
    driver.apply(AgentDirective::CheckGroup { process: ONE });
    let events = until(&mut driver, ONE, |events| !events.is_empty()).await;
    assert!(
        matches!(&events[..], [AgentEvent::StartFailed { .. }]),
        "{events:?}"
    );
}

#[tokio::test]
async fn environment_and_folder_reach_the_process() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    let mut command = sh(r#"printf '%s|%s|%s|' "$CAF_A" "${CAF_B-unset}" "$PATH" >&2; pwd >&2"#);
    command.env = vec![
        EnvChange::Set("CAF_A".into(), "1".into()),
        EnvChange::Set("CAF_B".into(), "2".into()),
        EnvChange::Remove("CAF_B".into()),
        EnvChange::Set("PATH".into(), "rel:/usr/bin:.:/bin".into()),
    ];
    command.cwd = Some("/".into());
    start(&mut driver, ONE, command);
    let events = until_done(&mut driver, ONE).await;
    assert_eq!(
        String::from_utf8(stderr_of(&events)).unwrap(),
        "1|unset|/usr/bin:/bin|/\n"
    );
}

#[tokio::test]
async fn nothing_is_pending_without_processes() {
    let mut driver = AgentDriver::new(AgentLimits::default());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), driver.next())
            .await
            .is_err()
    );
}
