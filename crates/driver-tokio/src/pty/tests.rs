use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use caf_core::ids::CallId;
use caf_tools::Screen;

use super::*;

/// How long anything here may take, at most: long, as a loaded machine
/// is slow, and only a failure waits it out.
const PATIENCE: Duration = Duration::from_secs(30);

/// A folder of the test's own in the temporary folder, removed when
/// dropped.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static NO: AtomicU64 = AtomicU64::new(0);
        let no = NO.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("cp{}-{no}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        // As the shell names it.
        Self(std::fs::canonicalize(&path).unwrap())
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What a command printed, as it comes.
#[derive(Clone, Default)]
struct Printed(Arc<Mutex<Vec<u8>>>);

impl Printed {
    fn out(&self) -> Box<dyn FnMut(Output) + Send> {
        let printed = self.0.clone();
        Box::new(move |output| match output {
            Output::Bytes(bytes) => printed.lock().unwrap().extend_from_slice(&bytes),
            other => panic!("the pty gives bytes, not {other:?}"),
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }

    /// Waits for a line that's a number, a process's id, to be printed.
    async fn pid(&self) -> u32 {
        tokio::time::timeout(PATIENCE, async {
            loop {
                if let Some(pid) = (self.text().lines()).find_map(|line| line.trim().parse().ok()) {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a pid was printed")
    }
}

async fn start_in(command: &str, folder: &Path) -> (Box<dyn caf_tools::Running>, Printed) {
    let printed = Printed::default();
    let spec = CommandSpec::new(command, folder, CallId(1));
    let running = PtyCommands
        .start(spec, printed.out())
        .await
        .expect("it starts");
    (running, printed)
}

/// Runs `command` in `folder` to its end.
async fn run_in(command: &str, folder: &Path) -> (Ended, String) {
    let (mut running, printed) = start_in(command, folder).await;
    let ended = tokio::time::timeout(PATIENCE, running.wait())
        .await
        .expect("it ended in time");
    (ended, printed.text())
}

async fn run(command: &str) -> (Ended, String) {
    let dir = Dir::new();
    run_in(command, &dir.0).await
}

/// Waits for process `pid` to be gone.
async fn gone(pid: u32) {
    let pid = libc::pid_t::try_from(pid).unwrap();
    tokio::time::timeout(PATIENCE, async {
        loop {
            #[allow(unsafe_code)]
            // SAFETY: a plain system call on numbers.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the process went");
}

#[tokio::test]
async fn a_command_says_how_it_exited_and_what_it_printed() {
    let (ended, text) = run("echo hi; echo there").await;
    assert_eq!(ended.exit, Some(Exit::Code(0)));
    // A pty's line ends.
    assert_eq!(text, "hi\r\nthere\r\n");
    assert_eq!(run("exit 3").await.0.exit, Some(Exit::Code(3)));
    assert_eq!(run("false").await.0.exit, Some(Exit::Code(1)));
    assert_eq!(run("kill -TERM $$").await.0.exit, Some(Exit::Signal(15)));
    // The command is all there is of the shell's arguments.
    assert_eq!(run("echo \"[$#]\"").await.1, "[0]\r\n");
}

#[tokio::test]
async fn the_folder_the_shell_ended_in_comes_back_whatever_its_exit() {
    let dir = Dir::new();
    std::fs::create_dir(dir.0.join("sub")).unwrap();
    let (ended, _) = run_in("true", &dir.0).await;
    assert_eq!(ended.folder.as_deref(), Some(dir.0.as_path()));
    let (ended, _) = run_in("cd sub && echo moved", &dir.0).await;
    assert_eq!(ended.folder, Some(dir.0.join("sub")));
    // Its status is the command's own.
    let (ended, _) = run_in("cd sub; exit 4", &dir.0).await;
    assert_eq!(
        (ended.exit, ended.folder),
        (Some(Exit::Code(4)), Some(dir.0.join("sub")))
    );
    // Nothing it starts holds the folder's pipe.
    let (ended, _) = run_in("ls -l /proc/self/fd/3 2>&1 | grep -c pipe", &dir.0).await;
    assert_eq!(ended.exit, Some(Exit::Code(1)), "fd 3 is closed for it");
}

#[tokio::test]
async fn a_folder_that_isnt_there_is_said_so() {
    let dir = Dir::new();
    let spec = CommandSpec::new("true", dir.0.join("none"), CallId(1));
    let error = PtyCommands
        .start(spec, Printed::default().out())
        .await
        .err()
        .expect("it doesn't start");
    assert!(error.contains("doesn't exist"), "{error}");
}

#[tokio::test]
async fn it_runs_in_a_terminal_the_size_of_the_screen() {
    let (_, text) = run("test -t 0 && test -t 1 && echo tty; stty size").await;
    assert_eq!(text, "tty\r\n24 200\r\n");
    // Its own session and group, which the terminal is the controlling
    // one of.
    let (ended, _) =
        run("test \"$(ps -o sid= -p $$)\" -eq $$ && test \"$(ps -o pgid= -p $$)\" -eq $$").await;
    assert_eq!(ended.exit, Some(Exit::Code(0)));
}

#[tokio::test]
async fn a_progress_bar_is_its_last_drawing_on_the_screen() {
    let (_, text) =
        run("printf 'start\\n'; for n in 10 50 100; do printf '\\r%s%%' $n; done; printf '\\n'")
            .await;
    let mut screen = Screen::new();
    let mut lines: Vec<String> = Vec::new();
    for redraw in [screen.feed(text.as_bytes()), screen.finish()]
        .into_iter()
        .flatten()
    {
        lines.truncate(usize::try_from(redraw.from).unwrap());
        lines.extend(redraw.lines);
    }
    assert_eq!(lines[..2], ["start", "100%"]);
}

#[tokio::test]
async fn stopping_a_command_stops_everything_it_started() {
    // One ignores SIGTERM, and the hangup as its shell goes: SIGKILL ends
    // it.
    let command = "sleep 1000 & echo $!; (trap '' TERM HUP; exec sleep 1001) & echo $!; wait";
    let dir = Dir::new();
    let (mut running, printed) = start_in(command, &dir.0).await;
    let first = printed.pid().await;
    // Both are started.
    tokio::time::timeout(PATIENCE, async {
        while printed.text().lines().count() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let second: u32 = printed
        .text()
        .lines()
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    tokio::time::timeout(PATIENCE, running.stop())
        .await
        .unwrap();
    gone(first).await;
    gone(second).await;
    let ended = running.wait().await;
    assert!(matches!(ended.exit, Some(Exit::Signal(_))), "{ended:?}");
    assert!(!printed.text().contains("Bad file"), "{}", printed.text());
}

#[tokio::test]
async fn dropping_a_command_stops_it_without_waiting() {
    let dir = Dir::new();
    let (running, printed) = start_in("sleep 1000 & echo $!; wait", &dir.0).await;
    let pid = printed.pid().await;
    drop(running);
    gone(pid).await;
}

#[tokio::test]
async fn a_command_that_leaves_something_behind_still_ends() {
    // Its terminal's hangup ends what it left, or the drain gives up on
    // it: either way, the command's end isn't held up.
    let (ended, _) = run("sleep 1000 &").await;
    assert_eq!(ended.exit, Some(Exit::Code(0)));
}

#[tokio::test]
async fn an_interrupt_goes_to_the_commands_group() {
    let dir = Dir::new();
    let (mut running, printed) = start_in("echo started; sleep 1000", &dir.0).await;
    tokio::time::timeout(PATIENCE, async {
        while !printed.text().contains("started") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    running.interrupt();
    let ended = tokio::time::timeout(PATIENCE, running.wait())
        .await
        .unwrap();
    assert_ne!(ended.exit, Some(Exit::Code(0)));
}

#[test]
fn its_environment_is_the_apps_with_terminal_and_pager_set_and_caf_and_claude_code_left_out() {
    let spec = CommandSpec::new("true", "/", CallId(1)).with_env("MINE", "1");
    let app = [
        ("HOME", "/home/me"),
        ("PATH", "/usr/bin:/bin"),
        ("CLAUDECODE", "1"),
        ("CAF_RELAY", "x"),
        ("CAF_RELAY_SOCKET", "x"),
        ("CAF_OTHER", "kept"),
        ("TERM", "dumb"),
        ("PAGER", "less"),
    ]
    .map(|(name, value)| (OsString::from(name), OsString::from(value)));
    let command = shell_command(&spec, app.into_iter());
    let mut env: Vec<(String, String)> = command
        .get_envs()
        .filter_map(|(name, value)| Some((name.to_str()?.to_owned(), value?.to_str()?.to_owned())))
        .collect();
    env.sort();
    let expected = [
        ("CAF_OTHER", "kept"),
        ("GIT_PAGER", "cat"),
        ("HOME", "/home/me"),
        ("MINE", "1"),
        ("PAGER", "cat"),
        ("PATH", "/usr/bin:/bin"),
        ("TERM", "xterm-256color"),
    ]
    .map(|(name, value)| (name.to_owned(), value.to_owned()));
    assert_eq!(env, expected);
    // bash where it's found, sh otherwise.
    let program = command.get_program().to_string_lossy().into_owned();
    assert!(
        program.ends_with("/bash") || program == "/bin/sh",
        "{program}"
    );
    let none = [(OsString::from("PATH"), OsString::from("/nowhere"))];
    let command = shell_command(&spec, none.into_iter());
    assert_eq!(command.get_program(), "/bin/sh");
}
