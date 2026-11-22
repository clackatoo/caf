//! What the tests share.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A folder of a test's own, in the system's temporary folder, removed
/// when it's done with.
pub(crate) struct Folder(PathBuf);

impl Folder {
    pub(crate) fn new(name: &str) -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "caf-usage-{name}-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Makes a named pipe at `path`: what a reader that waits for a writer
/// would hang on.
pub(crate) fn pipe(path: &Path) {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        path,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .expect("the pipe is made");
}

/// What `work` gives, failing the test if it takes longer than `limit`:
/// so a test of something that mustn't hang fails rather than hangs.
pub(crate) fn within<T: Send + 'static>(
    limit: std::time::Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(work());
    });
    receive
        .recv_timeout(limit)
        .expect("it ends in time, not hanging")
}

/// Takes the lock file at `path`, exclusively, through a file of its own:
/// another holder, as far as `flock` goes, though in this program.
pub(crate) fn hold(path: &Path) -> std::fs::File {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .expect("the lock opens");
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).expect("it's held");
    file
}
