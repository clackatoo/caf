//! [`PtyCommands`]: CAF's tools' commands, each in a pty of its own.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use caf_core::event::Exit;
use caf_tools::{BoxFuture, CommandSpec, Commands, Ended, Output};
use rustix::process::{Pid, PidfdFlags, WaitId, WaitIdOptions, WaitIdStatus};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::agent::{PIDFD_SIGNAL_PROCESS_GROUP, pidfd_send_signal};

/// How long a stopped command has after SIGTERM before SIGKILL.
pub const STOP_GRACE: Duration = Duration::from_secs(3);
/// How long the pty is read after its command ended, for what's still
/// on its way, if something it left running holds it open.
const DRAIN: Duration = Duration::from_millis(500);
/// The most read of the pipe the shell's folder comes through.
const MOST_FOLDER: usize = 64 * 1024;
/// How much of the pty is read at once.
const READ_SIZE: usize = 64 * 1024;

/// What a command is run in, the folder it ends in written to fd 3 as it
/// exits. The command is `$1`, shifted away before it runs, which keeps
/// fd 3 closed, so nothing it starts holds that pipe open. The `EXIT`
/// trap keeps the command's status.
const WRAPPER: &str = "trap '{ pwd -P >&3; } 2>/dev/null' EXIT\neval \"shift; $1\" 3>&-";

/// The commands CAF's Bash runs, each in a pty of its own: the default
/// [`ToolsConfig::commands`](crate::ToolsConfig::commands).
///
/// - **A pty** the size of [`caf_tools::Screen`]'s window, with the
///   command in a session and process group of its own, the pty its
///   controlling terminal.
/// - **`bash -c`**, or `sh -c` where there's no bash, so it's not
///   interactive: what it starts in the background stays in its group.
/// - **Its environment** is the app's, with `TERM=xterm-256color`,
///   `PAGER=cat` and `GIT_PAGER=cat`, without `CLAUDECODE` or any
///   `CAF_RELAY*`, and with the spec's own variables.
/// - **The folder its shell ended in** comes back in [`Ended`].
/// - **Stopping** sends SIGTERM to its group, then SIGKILL after
///   [`STOP_GRACE`] to what's left, through its pidfd (on Linux 6.9 or
///   later; `kill(-pgid)` before). A command that ended by itself isn't
///   signalled: what it left in the background has the hangup its
///   terminal's end sends. Dropping it stops it the same way, without
///   waiting. It's always reaped.
#[derive(Clone, Copy, Debug, Default)]
pub struct PtyCommands;

impl Commands for PtyCommands {
    fn start(
        &self,
        spec: CommandSpec,
        out: Box<dyn FnMut(Output) + Send>,
    ) -> BoxFuture<Result<Box<dyn caf_tools::Running>, String>> {
        let started =
            start(&spec, out).map(|running| Box::new(running) as Box<dyn caf_tools::Running>);
        Box::pin(std::future::ready(started))
    }
}

/// What a command's task is told.
#[derive(Clone, Copy, Debug)]
enum Control {
    Interrupt,
    Stop,
}

/// A command running in a pty.
struct PtyRunning {
    control: mpsc::UnboundedSender<Control>,
    ended: watch::Receiver<Option<Ended>>,
}

impl caf_tools::Running for PtyRunning {
    fn wait(&mut self) -> BoxFuture<Ended> {
        let mut ended = self.ended.clone();
        Box::pin(async move {
            match ended.wait_for(Option::is_some).await {
                Ok(ended) => ended.clone().unwrap_or_else(lost),
                Err(_) => lost(),
            }
        })
    }

    fn interrupt(&mut self) {
        let _ = self.control.send(Control::Interrupt);
    }

    fn stop(&mut self) -> BoxFuture<()> {
        let _ = self.control.send(Control::Stop);
        let wait = self.wait();
        Box::pin(async move {
            wait.await;
        })
    }

    fn place(&self) -> Option<String> {
        None
    }
}

impl Drop for PtyRunning {
    /// Stops it, without waiting: its task carries on until it's done.
    fn drop(&mut self) {
        let _ = self.control.send(Control::Stop);
    }
}

/// How a command ended when its task went before it could say.
fn lost() -> Ended {
    Ended::new(None, None)
}

/// Starts `spec`'s command in a pty, its output going to `out`.
fn start(spec: &CommandSpec, out: Box<dyn FnMut(Output) + Send>) -> Result<PtyRunning, String> {
    let (master, slave) =
        open_pty().map_err(said("CAF couldn't open a terminal for the command"))?;
    let (folder_read, folder_write) = rustix::pipe::pipe_with(
        rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
    )
    .map_err(|errno| said("CAF couldn't open a pipe for the command")(errno.into()))?;
    // Its end is the child's, blocking.
    rustix::fs::fcntl_setfl(&folder_write, rustix::fs::OFlags::empty())
        .map_err(|errno| said("CAF couldn't open a pipe for the command")(errno.into()))?;
    let mut command = shell_command(spec, std::env::vars_os());
    let streams = (|| Ok::<_, io::Error>((slave.try_clone()?, slave.try_clone()?, slave)))();
    let (stdin, stdout, stderr) = streams.map_err(said("CAF couldn't start the command"))?;
    command
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let folder_fd = folder_write.as_raw_fd();
    // SAFETY: the closure runs in the child between fork and exec, and
    // makes only async-signal-safe system calls on fds it was given.
    #[allow(unsafe_code)]
    unsafe {
        command.pre_exec(move || child_setup(folder_fd));
    }
    let child = command.spawn().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound && !spec.folder.is_dir() {
            format!("The folder {} doesn't exist", spec.folder.display())
        } else {
            format!("CAF couldn't start the command: {error}")
        }
    })?;
    // The child has its ends: the pipe's and the pty's must close here,
    // so their ends here hear when it's done with them.
    drop(command);
    drop(folder_write);
    let pid = child.id();
    let pidfd = match rustix::process::pidfd_open(Pid::from_child(&child), PidfdFlags::empty()) {
        Ok(pidfd) => pidfd,
        Err(errno) => {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "CAF couldn't watch the command: {}",
                io::Error::from(errno)
            ));
        }
    };
    // From here on it's reaped through its pidfd, and the guard kills
    // and reaps it if anything goes wrong.
    drop(child);
    let pidfd = match watch_fd(pidfd, Interest::READABLE) {
        Ok(pidfd) => pidfd,
        Err((pidfd, error)) => {
            // Killed and reaped as it's dropped.
            drop(Leader {
                pid,
                pidfd: None,
                unregistered: Some(pidfd),
            });
            return Err(format!("CAF couldn't watch the command: {error}"));
        }
    };
    let leader = Leader {
        pid,
        pidfd: Some(pidfd),
        unregistered: None,
    };
    let watched = (|| -> io::Result<_> {
        rustix::fs::fcntl_setfl(&master, rustix::fs::OFlags::NONBLOCK)?;
        let master = watch_fd(master, Interest::READABLE).map_err(|(_, error)| error)?;
        let folder = watch_fd(folder_read, Interest::READABLE).map_err(|(_, error)| error)?;
        Ok((master, folder))
    })();
    let (master, folder) = watched.map_err(said("CAF couldn't watch the command"))?;
    let (control, controls) = mpsc::unbounded_channel();
    let (ended_tx, ended) = watch::channel(None);
    tokio::spawn(watch_command(Watched {
        leader,
        master: Some(master),
        folder: Some(folder),
        out,
        controls,
        ended: ended_tx,
    }));
    Ok(PtyRunning { control, ended })
}

/// What's said of `error`, as `what` failed.
fn said(what: &'static str) -> impl Fn(io::Error) -> String {
    move |error| format!("{what}: {error}")
}

/// `fd`, watched by tokio for `interest`: or, if it can't be, it back,
/// and why.
fn watch_fd(fd: OwnedFd, interest: Interest) -> Result<AsyncFd<OwnedFd>, (OwnedFd, io::Error)> {
    // SAFETY: an `OwnedFd` stays open, and is the same fd, for as long as
    // it's owned.
    #[allow(unsafe_code)]
    let registered = unsafe { AsyncFd::register_with_interest(fd, interest) };
    registered.map_err(|error| error.into_parts())
}

/// A new pty: its master, and its slave, sized as the screen is.
fn open_pty() -> io::Result<(OwnedFd, OwnedFd)> {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let name = ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let size = rustix::termios::Winsize {
        ws_row: caf_tools::ROWS as u16,
        ws_col: caf_tools::COLUMNS as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    rustix::termios::tcsetwinsize(&slave, size)?;
    Ok((master, slave))
}

/// In the child, before it runs the shell: a session of its own, whose
/// controlling terminal is the pty on its standard streams, and the
/// folder's pipe as fd 3.
fn child_setup(folder_fd: i32) -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: plain system calls on numbers, async-signal-safe.
    unsafe {
        if libc::setsid() < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
        if folder_fd == 3 {
            // dup2 to itself leaves close-on-exec set.
            if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
        } else if libc::dup2(folder_fd, 3) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The shell that runs `spec`'s command, in its folder, with the
/// environment `env` (the app's) made as [`PtyCommands`] says.
fn shell_command(spec: &CommandSpec, env: impl Iterator<Item = (OsString, OsString)>) -> Command {
    let env: Vec<(OsString, OsString)> = env.collect();
    let path = env
        .iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value.clone());
    let shell = find_bash(path.as_deref()).unwrap_or_else(|| PathBuf::from("/bin/sh"));
    let mut command = Command::new(&shell);
    command
        .arg("-c")
        .arg(WRAPPER)
        .arg(shell.file_name().unwrap_or(shell.as_os_str()))
        .arg(&spec.command)
        .current_dir(&spec.folder)
        .env_clear();
    for (name, value) in &env {
        if kept(name) {
            command.env(name, value);
        }
    }
    command
        .env("TERM", "xterm-256color")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat");
    for (name, value) in &spec.env {
        command.env(name, value);
    }
    command
}

/// Whether the app's variable `name` is passed on to a command.
fn kept(name: &OsStr) -> bool {
    let name = name.as_bytes();
    name != b"CLAUDECODE" && !name.starts_with(b"CAF_RELAY")
}

/// bash, if it's in a folder of `path`.
fn find_bash(path: Option<&OsStr>) -> Option<PathBuf> {
    let path = path?;
    (path.as_bytes().split(|&byte| byte == b':'))
        .filter(|dir| dir.starts_with(b"/"))
        .map(|dir| Path::new(OsStr::from_bytes(dir)).join("bash"))
        .find(|bash| is_executable(bash))
}

fn is_executable(path: &Path) -> bool {
    rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok() && path.is_file()
}

/// The process leading a command's group, known by its pidfd: killed and
/// reaped if it's dropped before it was, so nothing is left running, nor a
/// zombie.
struct Leader {
    pid: u32,
    /// `None` once reaped.
    pidfd: Option<AsyncFd<OwnedFd>>,
    /// Its pidfd, if it couldn't be watched.
    unregistered: Option<OwnedFd>,
}

impl Leader {
    /// Sends `signal` to its group: through its pidfd while it's not
    /// reaped, on Linux 6.9 or later, else to the group's id.
    fn signal(&self, signal: libc::c_int) {
        if let Some(pidfd) = &self.pidfd
            && pidfd_send_signal(pidfd.get_ref().as_fd(), signal, PIDFD_SIGNAL_PROCESS_GROUP)
                .is_ok()
        {
            return;
        }
        kill_group(self.pid, signal);
    }

    /// Whether anything is left in its group.
    fn group_alive(&self) -> bool {
        group_alive(self.pid)
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        let pidfd = match (self.pidfd.take(), self.unregistered.take()) {
            (Some(pidfd), _) => pidfd.into_inner(),
            (None, Some(pidfd)) => pidfd,
            (None, None) => return,
        };
        let _ = pidfd_send_signal(pidfd.as_fd(), libc::SIGKILL, PIDFD_SIGNAL_PROCESS_GROUP);
        kill_group(self.pid, libc::SIGKILL);
        let _ = pidfd_send_signal(pidfd.as_fd(), libc::SIGKILL, 0);
        let reap = move || {
            let _ = rustix::process::waitid(WaitId::PidFd(pidfd.as_fd()), WaitIdOptions::EXITED);
        };
        // Not on the runtime's thread.
        if let Err(error) = std::thread::Builder::new()
            .name("caf-reap".to_owned())
            .spawn(reap)
        {
            log::warn!(target: "caf", "a command's process couldn't be reaped: {error}");
        }
    }
}

/// Sends `signal` to process group `pgid`.
fn kill_group(pgid: u32, signal: libc::c_int) {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return;
    };
    if pgid > 1 {
        #[allow(unsafe_code)]
        // SAFETY: a plain system call on numbers.
        unsafe {
            libc::kill(-pgid, signal);
        }
    }
}

/// Whether anything is left in process group `pgid`.
fn group_alive(pgid: u32) -> bool {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return false;
    };
    #[allow(unsafe_code)]
    // SAFETY: a plain system call on numbers.
    let sent = unsafe { libc::kill(-pgid, 0) };
    sent == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// A command, as its task watches it.
struct Watched {
    leader: Leader,
    master: Option<AsyncFd<OwnedFd>>,
    folder: Option<AsyncFd<OwnedFd>>,
    out: Box<dyn FnMut(Output) + Send>,
    controls: mpsc::UnboundedReceiver<Control>,
    ended: watch::Sender<Option<Ended>>,
}

/// Watches a command until it's done: reads its pty and its folder's
/// pipe, signals it as it's told, reaps it, and says how it ended.
async fn watch_command(mut watched: Watched) {
    let mut status: Option<Option<WaitIdStatus>> = None;
    let mut folder = Vec::new();
    let mut buffer = vec![0u8; READ_SIZE];
    // When the group is killed, once it's stopping.
    let mut kill_at: Option<Instant> = None;
    let mut killed = false;
    // When the pty stops being read, once the command has ended.
    let mut drain_until: Option<Instant> = None;
    let mut controls_open = true;
    loop {
        if status.is_some() && watched.folder.is_none() {
            // Stopping: until everything in its group has gone.
            let group_done = kill_at.is_none() || !watched.leader.group_alive();
            if (watched.master.is_none() || drain_until.is_some_and(|at| at <= Instant::now()))
                && group_done
            {
                break;
            }
        }
        let pidfd = watched.leader.pidfd.as_ref();
        tokio::select! {
            read = readable(watched.master.as_ref()) => {
                match read {
                    Ok(mut guard) => match rustix::io::read(guard.get_inner(), &mut buffer) {
                        Ok(0) => watched.master = None,
                        Ok(n) => (watched.out)(Output::Bytes(buffer[..n].to_vec())),
                        Err(rustix::io::Errno::AGAIN) => guard.clear_ready(),
                        Err(rustix::io::Errno::INTR) => {}
                        // EIO: nothing holds the pty open any more.
                        Err(_) => watched.master = None,
                    },
                    Err(_) => watched.master = None,
                }
            }
            read = readable(watched.folder.as_ref()) => {
                let mut chunk = [0u8; 4096];
                match read {
                    Ok(mut guard) => match rustix::io::read(guard.get_inner(), &mut chunk) {
                        Ok(0) => watched.folder = None,
                        Ok(n) => {
                            if folder.len() < MOST_FOLDER {
                                folder.extend_from_slice(&chunk[..n]);
                            }
                        }
                        Err(rustix::io::Errno::AGAIN) => guard.clear_ready(),
                        Err(rustix::io::Errno::INTR) => {}
                        Err(_) => watched.folder = None,
                    },
                    Err(_) => watched.folder = None,
                }
            }
            waited = exit(pidfd), if status.is_none() => {
                match waited {
                    Ok(waited) => {
                        status = Some(waited);
                        watched.leader.pidfd = None;
                        drain_until = Some(Instant::now() + DRAIN);
                    }
                    // It can't be watched, as the runtime is going: the
                    // leader kills and reaps it as it's dropped.
                    Err(_) => {
                        let _ = watched.ended.send(Some(lost()));
                        return;
                    }
                }
            }
            control = watched.controls.recv(), if controls_open => match control {
                Some(Control::Interrupt) => watched.leader.signal(libc::SIGINT),
                Some(Control::Stop) => {
                    if kill_at.is_none() {
                        watched.leader.signal(libc::SIGTERM);
                        kill_at = Some(Instant::now() + STOP_GRACE);
                    }
                }
                None => controls_open = false,
            },
            () = sleep_until(kill_at.filter(|_| !killed)) => {
                killed = true;
                watched.leader.signal(libc::SIGKILL);
                if let Some(pidfd) = &watched.leader.pidfd {
                    let _ = pidfd_send_signal(pidfd.get_ref().as_fd(), libc::SIGKILL, 0);
                }
            }
            () = sleep_until(drain_until), if watched.master.is_some() => {
                watched.master = None;
            }
            // Stopping, it's looked at again shortly for what's left of
            // its group.
            () = tokio::time::sleep(Duration::from_millis(50)), if status.is_some() && kill_at.is_some() => {}
        }
    }
    let exit = match status
        .flatten()
        .map(|status| (status.exit_status(), status.terminating_signal()))
    {
        Some((Some(code), _)) => Some(Exit::Code(code)),
        Some((None, Some(signal))) => Some(Exit::Signal(signal)),
        _ => None,
    };
    let folder = std::str::from_utf8(&folder)
        .ok()
        .map(|text| text.trim_end_matches('\n'))
        .filter(|text| text.starts_with('/') && !text.contains('\n'))
        .map(PathBuf::from);
    let _ = watched.ended.send(Some(Ended::new(exit, folder)));
}

/// Resolves when `fd` may be read, or never if there's none.
async fn readable(
    fd: Option<&AsyncFd<OwnedFd>>,
) -> io::Result<tokio::io::unix::AsyncFdReadyGuard<'_, OwnedFd>> {
    match fd {
        Some(fd) => fd.readable().await,
        None => std::future::pending().await,
    }
}

/// Resolves at `at`, or never if it's `None`.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Waits for the process `pidfd` is, to reap it: how it ended, or `None`
/// if something else reaped it. Never, if there's none.
async fn exit(pidfd: Option<&AsyncFd<OwnedFd>>) -> io::Result<Option<WaitIdStatus>> {
    let Some(pidfd) = pidfd else {
        return std::future::pending().await;
    };
    loop {
        let mut ready = pidfd.readable().await?;
        match rustix::process::waitid(
            WaitId::PidFd(pidfd.get_ref().as_fd()),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
        ) {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => ready.clear_ready(),
            Err(rustix::io::Errno::CHILD) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests;
