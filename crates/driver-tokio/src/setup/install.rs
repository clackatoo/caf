//! Surveying, installing and removing: the setup driver's work on disk.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use caf_core::agent::InstallFailure;
use caf_core::event::SetupEvent;
use caf_core::install::{Artifact, Install, NpmPackage, Platform, Unpack};
use caf_core::time::Wall;
use rustix::fs::FlockOperation;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::sync::oneshot;

use super::{Options, Reporter, hidden_name};
use crate::agent::absolute_only;

/// How often a lock is tried while waiting for it, and a sign-in's
/// `BROWSER` folder looked in.
pub(crate) const POLL: Duration = Duration::from_millis(100);

/// How long a download may go with nothing coming, from connecting to
/// its last byte, before it's given up as offline.
const IDLE: Duration = Duration::from_secs(60);

/// How long a stopped download is waited for before it's left to end by
/// itself.
const STOPPING: Duration = Duration::from_millis(500);

/// How often an install's progress is told, at most.
const PROGRESS: Duration = Duration::from_millis(500);

/// How long finding Node's version, or npm's, may take.
const PROBE: Duration = Duration::from_secs(30);

/// The lock every install and removal takes.
const LOCK: &str = ".lock";

/// Hidden folders: installs being built, and being removed.
const BUILD: &str = "build";
const TRASH: &str = "trash";

/// An agent's private folder ends with this.
const PRIVATE: &str = "-private";

/// Says what's installed, and what the machine has. Half-built installs
/// are removed, if nothing is installing. It makes nothing: with no root,
/// nothing is installed.
pub(super) async fn survey(root: PathBuf, reporter: Reporter) {
    let installed = {
        let root = root.clone();
        blocking(move || {
            if !root.is_dir() {
                return Ok(Vec::new());
            }
            // Cleaned only while the lock is held. One that can't even be
            // made, as in a root that can't be written to, leaves things
            // as they are: what's there is still said.
            if let Ok(Some(lock)) = try_lock(&root.join(LOCK), Lock::Exclusive) {
                clean(&root);
                drop(lock);
            }
            installed(&root)
        })
        .await
    };
    let installed = match installed {
        Ok(installed) => installed,
        Err(error) => {
            log::warn!(target: "caf", "can't look at the installs in {}: {error}", root.display());
            Vec::new()
        }
    };
    let used = (installed.iter())
        .filter_map(|name| Some((name.clone(), last_used(&root.join(name))?)))
        .collect();
    let (node, npm) = tokio::join!(node_version(), has_npm());
    reporter.last(SetupEvent::Surveyed {
        root,
        installed,
        node,
        npm,
        platform: Platform::current(),
        used,
    });
}

/// The folders an install makes: the install's, and the agent's private
/// folder if it has one.
pub(super) struct Folders {
    pub(super) folder: String,
    pub(super) private: Option<String>,
    /// About how many bytes the agent takes once installed.
    pub(super) size: u64,
}

/// Installs `install` as `root/folder`, making the agent's private folder
/// too.
pub(super) async fn install(
    root: PathBuf,
    folders: Folders,
    install: Install,
    options: Options,
    reporter: Reporter,
    mut stop: oneshot::Receiver<()>,
) {
    let outcome = installing(&root, &folders, install, &options, &reporter, &mut stop).await;
    reporter.last(match outcome {
        Ok(()) => SetupEvent::Installed,
        Err(failure) => SetupEvent::InstallFailed(failure),
    });
}

/// Installs, holding `.lock` throughout, and removes what it built if it
/// fails.
async fn installing(
    root: &Path,
    folders: &Folders,
    install: Install,
    options: &Options,
    reporter: &Reporter,
    stop: &mut oneshot::Receiver<()>,
) -> Result<(), InstallFailure> {
    let folder = folders.folder.as_str();
    if !is_install_name(folder) {
        return Err(other(format!("{folder:?} can't be an install's name")));
    }
    if let Some(private) = &folders.private
        && !is_private_name(private)
    {
        return Err(other(format!(
            "{private:?} can't be a private folder's name"
        )));
    }
    {
        let root = root.to_owned();
        blocking(move || make_root(&root)).await.map_err(failure)?;
    }
    let lock = wait_for_lock(root, stop).await?;
    let place = root.join(folder);
    let build = root.join(hidden_name(BUILD, folder));
    let prepared = {
        let (place, build, root) = (place.clone(), build.clone(), root.to_owned());
        let private = folders.private.as_ref().map(|private| root.join(private));
        blocking(move || {
            if let Some(private) = &private {
                make_private(private)?;
            }
            // Under the lock, so it's complete if it's there: someone
            // else installed it while this waited.
            if place.is_dir() {
                return Ok(false);
            }
            // Under the lock, so any hidden folder is left from a run
            // that was cut off.
            clean(&root);
            fs::DirBuilder::new().mode(0o755).create(&build)?;
            Ok(true)
        })
        .await
    };
    if !prepared.map_err(failure)? {
        return Ok(());
    }
    // What npm wrote is synced file by file before the rename; what a
    // download put there was synced as it was written.
    let files = matches!(install, Install::Npm(_));
    let built = match install {
        Install::Npm(package) => npm(&build, &package, options, reporter, stop).await,
        Install::Download(artifacts) => {
            download(&build, &artifacts, folders.size, options, reporter, stop).await
        }
        // No other kind is given yet.
        _ => Err(other("CAF doesn't know this kind of install".into())),
    };
    let done = match built {
        Ok(()) => placed(&build, &place, files, options, stop).await,
        Err(failure) => Err(failure),
    };
    if done.is_err() {
        blocking(move || remove_all(&build)).await.ok();
    }
    drop(lock);
    done
}

/// Removes `root/folder`, unless something runs from it.
pub(super) async fn remove(root: PathBuf, folder: String, unused_since: Wall, reporter: Reporter) {
    let removed = removing(&root, &folder, unused_since).await;
    let removed = removed.unwrap_or_else(|error| {
        log::warn!(target: "caf", "can't remove {folder} from {}: {error}", root.display());
        false
    });
    reporter.last(SetupEvent::Removed { removed });
}

/// Removes `root/folder`, unless it was used after `unused_since` or
/// something runs from it. Says whether it's gone.
async fn removing(root: &Path, folder: &str, unused_since: Wall) -> io::Result<bool> {
    if !is_install_name(folder) {
        return Err(io::Error::other(format!(
            "{folder:?} isn't an install's name"
        )));
    }
    // Never stopped: a removal waits for an install only.
    let (_never, mut stop) = oneshot::channel();
    let lock = wait_for_lock(root, &mut stop)
        .await
        .map_err(|_| io::Error::other("can't take the installs' lock"))?;
    let place = root.join(folder);
    let trash = root.join(hidden_name(TRASH, folder));
    let moved = blocking(move || {
        let held = match File::open(&place) {
            Ok(held) => held,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        // Used since the survey the core decided on: it stays. Read under
        // the installs' lock, so nothing installs it again meanwhile.
        if last_used(&place).is_none_or(|used| used > unused_since) {
            return Ok(Some(None));
        }
        // Something runs from it: it stays.
        let Some(held) = lock_file(held, Lock::ExclusiveNow)? else {
            return Ok(Some(None));
        };
        fs::rename(&place, &trash)?;
        drop(held);
        Ok(Some(Some(trash)))
    })
    .await;
    drop(lock);
    match moved? {
        // It wasn't there: it's gone.
        None => Ok(true),
        Some(None) => Ok(false),
        Some(Some(trash)) => {
            // Left behind if this fails: the next survey removes it.
            blocking(move || remove_all(&trash)).await.ok();
            Ok(true)
        }
    }
}

/// Puts a built install into place, as [`into_place`] does, while still
/// hearing Stop: the sync can take a while, and a stop then is a stop
/// like any other.
async fn placed(
    build: &Path,
    place: &Path,
    files: bool,
    options: &Options,
    stop: &mut oneshot::Receiver<()>,
) -> Result<(), InstallFailure> {
    let stopped = Arc::new(AtomicBool::new(false));
    let placing = {
        let (build, place, stopped) = (build.to_owned(), place.to_owned(), Arc::clone(&stopped));
        let pause = sync_pause(options);
        tokio::task::spawn_blocking(move || into_place(&build, &place, files, pause, &stopped))
    };
    tokio::pin!(placing);
    let placed = tokio::select! {
        placed = &mut placing => placed,
        _ = &mut *stop => {
            // It sees that between files, and ends without the rename.
            stopped.store(true, Ordering::Relaxed);
            placing.await
        }
    };
    match placed {
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false)) => Err(InstallFailure::Stopped),
        Ok(Err(error)) => Err(failure(error)),
        Err(_) => Err(other("putting the install in place panicked".into())),
    }
}

/// How long syncing each file takes, as well: only tests make it longer.
fn sync_pause(options: &Options) -> Duration {
    #[cfg(test)]
    if let Some(pause) = options.sync_pause {
        return pause;
    }
    let _ = options;
    Duration::ZERO
}

/// Renames a built install into place, once all it holds is on the disk:
/// a power cut must never leave an install that looks whole and isn't.
/// It's synced (its files too if `files`, as when npm wrote them; others
/// were synced as they were written), then renamed, and the folder it's
/// renamed in synced so the rename lasts too. Only its own are synced, not
/// the whole disk, so it never waits on what others write. Returns `false`
/// if `stopped` before the rename, which then isn't done.
fn into_place(
    build: &Path,
    place: &Path,
    files: bool,
    pause: Duration,
    stopped: &AtomicBool,
) -> io::Result<bool> {
    if !sync_all_in(build, files, pause, stopped)? {
        return Ok(false);
    }
    fs::rename(build, place)?;
    if let Some(root) = place.parent() {
        File::open(root)?.sync_all()?;
    }
    Ok(true)
}

/// Syncs every folder under `folder`, and `folder` itself, to the disk,
/// and every file too if `files`, until `stopped`: then it returns `false`.
/// Links are left as they are: what they lead to is synced where it is.
fn sync_all_in(
    folder: &Path,
    files: bool,
    pause: Duration,
    stopped: &AtomicBool,
) -> io::Result<bool> {
    let mut folders = vec![folder.to_owned()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder)? {
            if stopped.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                folders.push(entry.path());
            } else if files && kind.is_file() {
                File::open(entry.path())?.sync_all()?;
                if !pause.is_zero() {
                    std::thread::sleep(pause);
                }
            }
        }
        File::open(&folder)?.sync_all()?;
    }
    Ok(!stopped.load(Ordering::Relaxed))
}

/// Whether `name` can be an install's folder: one plain name, not hidden,
/// and not an agent's private folder.
fn is_install_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    ) && !name.starts_with('.')
        && !name.ends_with(PRIVATE)
}

/// Whether `name` can be an agent's private folder: one plain name, not
/// hidden, ending in `-private`.
fn is_private_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    ) && !name.starts_with('.')
        && name.len() > PRIVATE.len()
        && name.ends_with(PRIVATE)
}

/// Makes an agent's private folder, readable only by the user, unless it's
/// there. One that's a link, or not a folder, is refused: what's at the
/// other end of a link isn't CAF's to give an agent.
fn make_private(private: &Path) -> io::Result<()> {
    let refuse = |why: &str| Err(io::Error::other(format!("{} {why}", private.display())));
    match fs::symlink_metadata(private) {
        Ok(found) if found.file_type().is_symlink() => refuse("is a link"),
        Ok(found) if found.is_dir() => Ok(()),
        Ok(_) => refuse("isn't a folder"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::DirBuilder::new().mode(0o700).create(private) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => make_private(private),
                made => made,
            }
        }
        Err(error) => Err(error),
    }
}

/// Makes the root if it isn't there, readable only by the user.
fn make_root(root: &Path) -> io::Result<()> {
    if root.is_dir() {
        return Ok(());
    }
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(root) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        made => made,
    }
}

/// The complete installs in `root`: every folder that isn't hidden or an
/// agent's private folder.
fn installed(root: &Path) -> io::Result<Vec<String>> {
    let mut installed = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if is_install_name(&name) && entry.file_type()?.is_dir() {
            installed.push(name);
        }
    }
    installed.sort();
    Ok(installed)
}

/// When the install `folder` was last used: when its `.used` file was last
/// touched, as a process started from it, or, if it has none, when the
/// folder itself last changed, as it was installed. `None` if neither can
/// be read, so the core never removes it.
fn last_used(folder: &Path) -> Option<Wall> {
    let modified = (fs::metadata(folder.join(".used")))
        .or_else(|_| fs::metadata(folder))
        .and_then(|metadata| metadata.modified())
        .ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(Wall(i64::try_from(since.as_millis()).ok()?))
}

/// Removes hidden folders left by installs and removals that were cut
/// off. Only while holding `.lock`.
fn clean(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&format!(".{BUILD}-")) || name.starts_with(&format!(".{TRASH}-")) {
            let _ = remove_all(&entry.path());
        }
    }
}

/// Removes a folder and all in it, if it's there.
fn remove_all(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        removed => removed,
    }
}

/// How to lock.
#[derive(Clone, Copy)]
pub(crate) enum Lock {
    /// Shared, without waiting.
    SharedNow,
    /// Exclusive, without waiting.
    ExclusiveNow,
    /// Exclusive, without waiting: for `.lock`, which is made if missing.
    Exclusive,
}

/// Locks `file` as `how` says, without waiting: `None` if something else
/// holds a lock that's in the way.
pub(crate) fn lock_file(file: File, how: Lock) -> io::Result<Option<File>> {
    let operation = match how {
        Lock::SharedNow => FlockOperation::NonBlockingLockShared,
        Lock::ExclusiveNow | Lock::Exclusive => FlockOperation::NonBlockingLockExclusive,
    };
    match rustix::fs::flock(&file, operation) {
        Ok(()) => Ok(Some(file)),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Takes the lock file at `path`, made if it isn't there, without
/// waiting.
fn try_lock(path: &Path, how: Lock) -> io::Result<Option<File>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    lock_file(file, how)
}

/// Waits for `root/.lock`, until it's stopped.
async fn wait_for_lock(
    root: &Path,
    stop: &mut oneshot::Receiver<()>,
) -> Result<File, InstallFailure> {
    let path = root.join(LOCK);
    loop {
        let tried = {
            let path = path.clone();
            blocking(move || try_lock(&path, Lock::Exclusive)).await
        };
        if let Some(lock) = tried.map_err(failure)? {
            return Ok(lock);
        }
        tokio::select! {
            _ = &mut *stop => return Err(InstallFailure::Stopped),
            () = tokio::time::sleep(POLL) => {}
        }
    }
}

/// Runs `npm ci` in `build` from `package`'s manifest and lockfile.
async fn npm(
    build: &Path,
    package: &NpmPackage,
    options: &Options,
    reporter: &Reporter,
    stop: &mut oneshot::Receiver<()>,
) -> Result<(), InstallFailure> {
    #[cfg(test)]
    let npm_given = options.npm.clone();
    #[cfg(not(test))]
    let npm_given = None;
    let Some(npm) = npm_given.or_else(|| on_path("npm")) else {
        return Err(other("npm isn't on PATH".into()));
    };
    {
        let (build, manifest, lockfile) = (
            build.to_owned(),
            package.manifest.clone(),
            package.lockfile.clone(),
        );
        blocking(move || {
            fs::write(build.join("package.json"), manifest)?;
            fs::write(build.join("package-lock.json"), lockfile)
        })
        .await
        .map_err(failure)?;
    }
    let mut command = tokio::process::Command::new(npm);
    command
        .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"])
        .current_dir(build)
        .env("PATH", absolute_only(&path_var()))
        .env("npm_config_update_notifier", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    #[cfg(test)]
    if let Some(cache) = &options.npm_offline_cache {
        command.arg("--offline").env("npm_config_cache", cache);
    }
    let _ = options;
    let mut child = command
        .spawn()
        .map_err(|error| other(format!("npm couldn't be started: {error}")))?;
    // Read as it comes, so a full pipe never holds npm up, keeping the
    // end, where npm says what went wrong.
    let mut errors = child.stderr.take();
    let mut said = tokio::spawn(async move {
        let mut kept = Vec::new();
        let mut buffer = vec![0; 8 << 10];
        while let Some(stderr) = &mut errors {
            match stderr.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    kept.extend_from_slice(&buffer[..n]);
                    if kept.len() > 64 << 10 {
                        kept.drain(..kept.len() - (32 << 10));
                    }
                }
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    });
    let mut ticks = tokio::time::interval(PROGRESS);
    let mut told = 0;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            _ = &mut *stop => {
                kill_group(&child);
                let _ = child.wait().await;
                return Err(InstallFailure::Stopped);
            }
            _ = ticks.tick() => {
                let build = build.to_owned();
                let done = blocking(move || Ok(size_of(&build))).await.unwrap_or(0);
                if done != told {
                    told = done;
                    reporter.send(SetupEvent::Progress { done });
                }
            }
        }
    };
    // Something npm started may hold its error output open: what it said
    // is waited for only a moment.
    let said = match tokio::time::timeout(PROGRESS, &mut said).await {
        Ok(said) => said.unwrap_or_default(),
        Err(_) => {
            said.abort();
            String::new()
        }
    };
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(npm_failure(&said, status.code())),
        Err(error) => Err(other(format!("npm couldn't be waited for: {error}"))),
    }
}

/// Why npm failed, from what it said: only its error code, never the rest,
/// which may name private registries and more.
fn npm_failure(said: &str, code: Option<i32>) -> InstallFailure {
    let offline = [
        "ENOTFOUND",
        "EAI_AGAIN",
        "ECONNREFUSED",
        "ECONNRESET",
        "ETIMEDOUT",
        "ENETUNREACH",
        "ENOTCACHED",
    ];
    if offline.iter().any(|code| said.contains(code)) {
        return InstallFailure::Offline;
    }
    if said.contains("ENOSPC") {
        return InstallFailure::NoSpace;
    }
    if said.contains("EINTEGRITY") {
        return InstallFailure::Corrupt;
    }
    let npm_code = (said.lines())
        .filter_map(|line| line.trim().strip_prefix("npm error code "))
        .chain(
            said.lines()
                .filter_map(|line| line.trim().strip_prefix("npm ERR! code ")),
        )
        .map(str::trim)
        .find(|code| {
            !code.is_empty()
                && code.len() <= 32
                && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    let exit = code.map_or_else(
        || "was killed".to_owned(),
        |code| format!("exited with {code}"),
    );
    match npm_code {
        Some(npm_code) => other(format!("npm ci {exit} ({npm_code})")),
        None => other(format!("npm ci {exit}")),
    }
}

/// Downloads the artifact for this machine into `build`, checks it, and
/// unpacks it there.
async fn download(
    build: &Path,
    artifacts: &[Artifact],
    size: u64,
    options: &Options,
    reporter: &Reporter,
    stop: &mut oneshot::Receiver<()>,
) -> Result<(), InstallFailure> {
    let here = Platform::current();
    let Some(artifact) = artifacts.iter().find(|artifact| artifact.platform == here) else {
        return Err(other(format!(
            "there's no build for {} on {}",
            here.os, here.arch
        )));
    };
    if !allowed_url(&artifact.url, options) {
        return Err(other("a download must be https".into()));
    }
    let file = build.join(".download");
    // Made here, so the fetch makes nothing in the build folder itself: it
    // can be left to end by itself once stopped, while the folder goes.
    let out = {
        let file = file.clone();
        blocking(move || {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(file)
        })
        .await
        .map_err(failure)?
    };
    let done = Arc::new(AtomicU64::new(0));
    let stopped = Arc::new(AtomicBool::new(false));
    let fetching = {
        let (done, stopped) = (Arc::clone(&done), Arc::clone(&stopped));
        let how = Fetch {
            url: artifact.url.clone(),
            proxy: proxy_for(&artifact.url, |name| env_var(options, name)),
            https_only: !allowed_http(options),
            idle: per_read(options),
            most: most_for(size),
        };
        tokio::task::spawn_blocking(move || fetch(&how, out, &done, &stopped))
    };
    tokio::pin!(fetching);
    let mut ticks = tokio::time::interval(PROGRESS);
    let mut told = 0;
    // The fetch gives up by itself when nothing comes for `idle`; this is
    // for when it can't, such as while finding its host.
    let mut heard = tokio::time::Instant::now();
    let fetched = loop {
        tokio::select! {
            fetched = &mut fetching => break fetched,
            _ = &mut *stop => {
                // The fetch sees it between reads, and ends. One waiting on
                // the network may not read again for a minute: it's left
                // to end by itself, writing only to its file, which goes
                // with the build folder.
                stopped.store(true, Ordering::Relaxed);
                let _ = tokio::time::timeout(STOPPING, &mut fetching).await;
                return Err(InstallFailure::Stopped);
            }
            _ = ticks.tick() => {
                let now = done.load(Ordering::Relaxed);
                if now != told {
                    told = now;
                    heard = tokio::time::Instant::now();
                    reporter.send(SetupEvent::Progress { done: now });
                } else if heard.elapsed() > idle(options) {
                    stopped.store(true, Ordering::Relaxed);
                    let _ = tokio::time::timeout(STOPPING, &mut fetching).await;
                    return Err(InstallFailure::Offline);
                }
            }
        }
    };
    let sha256 = fetched.unwrap_or_else(|_| Err(other("the download panicked".into())))?;
    if !sha256.eq_ignore_ascii_case(artifact.sha256.trim()) {
        return Err(InstallFailure::Corrupt);
    }
    // Unpacked while Stop is still heard, as unpacking syncs each file,
    // which takes a while.
    let stopped = Arc::new(AtomicBool::new(false));
    let unpacking = {
        let (build, unpack, url) = (build.to_owned(), artifact.unpack, artifact.url.clone());
        let (stopped, pause) = (Arc::clone(&stopped), sync_pause(options));
        tokio::task::spawn_blocking(move || {
            unpack_into(&build, &file, unpack, &url, &stopped, pause)
        })
    };
    tokio::pin!(unpacking);
    let unpacked = tokio::select! {
        unpacked = &mut unpacking => unpacked,
        _ = &mut *stop => {
            // It sees that between entries, and ends.
            stopped.store(true, Ordering::Relaxed);
            let _ = unpacking.await;
            return Err(InstallFailure::Stopped);
        }
    };
    unpacked.unwrap_or_else(|_| Err(other("unpacking panicked".into())))
}

/// The proxy a download of `url` goes through, if any, and the variable
/// that named it, chosen from the environment variables npm reads, as npm
/// chooses, from what `var` gives:
/// - for `https`, npm's own `npm_config_https_proxy` or
///   `npm_config_proxy` (in either case), then `HTTPS_PROXY`,
///   `https_proxy`, `HTTP_PROXY` or `http_proxy`, the first set; for
///   `http`, `npm_config_proxy`, then `HTTP_PROXY` or `http_proxy`;
/// - none if `npm_config_noproxy`, `NO_PROXY` or `no_proxy`, a list split
///   by commas, names the host or a domain it's in: `example.com` and
///   `.example.com` both match `example.com` and `dl.example.com`.
///
/// What's set in an `.npmrc` file isn't read.
pub(crate) fn proxy_for(
    url: &str,
    var: impl Fn(&str) -> Option<String>,
) -> Option<(&'static str, String)> {
    let first = |names: &[&'static str]| {
        (names.iter()).find_map(|&name| {
            let value = var(name).filter(|value| !value.trim().is_empty())?;
            Some((name, value))
        })
    };
    let lower = url.to_ascii_lowercase();
    let proxy = if lower.starts_with("https://") {
        first(&[
            "npm_config_https_proxy",
            "NPM_CONFIG_HTTPS_PROXY",
            "npm_config_proxy",
            "NPM_CONFIG_PROXY",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ])
    } else {
        first(&[
            "npm_config_proxy",
            "NPM_CONFIG_PROXY",
            "HTTP_PROXY",
            "http_proxy",
        ])
    }?;
    let host = host_of(&lower)?;
    let no_proxy = [
        "npm_config_noproxy",
        "NPM_CONFIG_NOPROXY",
        "NO_PROXY",
        "no_proxy",
    ];
    let skipped = first(&no_proxy).is_some_and(|(_, list)| {
        let host: Vec<&str> = host.split('.').rev().collect();
        (list.split(',')).any(|entry| {
            let entry = entry.trim().to_ascii_lowercase();
            let entry: Vec<&str> = entry
                .split('.')
                .filter(|part| !part.is_empty())
                .rev()
                .collect();
            !entry.is_empty()
                && entry
                    .iter()
                    .enumerate()
                    .all(|(at, part)| host.get(at) == Some(part))
        })
    });
    (!skipped).then_some(proxy)
}

/// The host a URL names, without its port or user.
fn host_of(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split_once(']')?.0
    } else {
        authority
            .split_once(':')
            .map_or(authority, |(host, _)| host)
    };
    (!host.is_empty()).then(|| host.to_owned())
}

/// The environment variable `name`, as the tests set it, or as this
/// program was given it.
fn env_var(options: &Options, name: &str) -> Option<String> {
    #[cfg(test)]
    if let Some(env) = &options.env {
        return env.get(name).cloned();
    }
    let _ = options;
    std::env::var(name).ok()
}

/// How long ureq waits for each read: as long as a download may go with
/// nothing coming.
fn per_read(options: &Options) -> Duration {
    #[cfg(test)]
    if let Some(per_read) = options.per_read {
        return per_read;
    }
    idle(options)
}

/// How long a download may go with nothing coming.
fn idle(options: &Options) -> Duration {
    #[cfg(test)]
    if let Some(idle) = options.idle {
        return idle;
    }
    let _ = options;
    IDLE
}

/// Whether downloads may come over plain `http` from this machine.
fn allowed_http(options: &Options) -> bool {
    #[cfg(test)]
    return options.http_to_this_machine;
    #[cfg(not(test))]
    {
        let _ = options;
        false
    }
}

/// Whether `url` may be downloaded from: `https`, or for tests `http` to
/// this machine.
fn allowed_url(url: &str, options: &Options) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return true;
    }
    allowed_http(options)
        && ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
            .iter()
            .any(|start| lower.starts_with(start))
}

/// How big a download of an agent that takes `size` bytes installed may
/// be: twice that, and a mebibyte, as what's downloaded is mostly no
/// bigger than what's installed.
fn most_for(size: u64) -> u64 {
    size.saturating_mul(2).saturating_add(1 << 20)
}

/// A download to fetch.
struct Fetch {
    url: String,
    /// The proxy, and the variable that named it.
    proxy: Option<(&'static str, String)>,
    https_only: bool,
    /// How long it may go with nothing coming.
    idle: Duration,
    /// The most it may be, in bytes: past that, it's cut off.
    most: u64,
}

/// Downloads `how`'s URL to `out`, counting into `done`, until it's done,
/// `stopped`, or past `how`'s most. Returns its SHA-256, in hex.
fn fetch(
    how: &Fetch,
    mut out: File,
    done: &AtomicU64,
    stopped: &AtomicBool,
) -> Result<String, InstallFailure> {
    let (url, https_only, idle) = (how.url.as_str(), how.https_only, how.idle);
    let proxy = how
        .proxy
        .as_ref()
        .map(|(name, value)| (*name, value.as_str()));
    // Never the proxy's value in what's said: it may hold a password.
    let proxy = proxy
        .map(|(name, proxy)| {
            ureq::Proxy::new(proxy)
                .map_err(|_| other(format!("the proxy {name} names can't be used")))
        })
        .transpose()?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(proxy)
        .https_only(https_only)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_per_read(Some(idle))
        .http_status_as_error(true)
        .build()
        .into();
    let response = agent.get(url).call().map_err(download_failure)?;
    let mut body = response.into_body();
    let mut reader = body.with_config().limit(u64::MAX).reader();
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 << 10];
    loop {
        if stopped.load(Ordering::Relaxed) {
            return Err(InstallFailure::Stopped);
        }
        let n = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_failure(&error, true)),
        };
        let so_far = done.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
        if so_far > how.most {
            return Err(other(
                "the download was far bigger than its agent should be".into(),
            ));
        }
        hash.update(&buffer[..n]);
        out.write_all(&buffer[..n])
            .map_err(|error| io_failure(&error, false))?;
    }
    out.sync_all().map_err(|error| io_failure(&error, false))?;
    Ok(hex(&hash.finalize()))
}

/// Why a download failed, by the kind of failure alone: what ureq says
/// may hold a URL a redirect gave, with its token.
fn download_failure(error: ureq::Error) -> InstallFailure {
    let why = match error {
        ureq::Error::StatusCode(code) => {
            return other(format!("the download answered HTTP {code}"));
        }
        ureq::Error::Io(error) => return io_failure(&error, true),
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Timeout(_) => {
            return InstallFailure::Offline;
        }
        ureq::Error::Tls(_)
        | ureq::Error::Rustls(_)
        | ureq::Error::Pem(_)
        | ureq::Error::TlsRequired => "its site's certificate couldn't be checked",
        ureq::Error::RequireHttpsOnly(_) => "it was sent somewhere that isn't https",
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            "it was sent elsewhere too often, or somewhere it can't go"
        }
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => "it was sent somewhere that isn't a link",
        ureq::Error::InvalidProxyUrl | ureq::Error::ConnectProxyFailed(_) => {
            "its proxy wouldn't take it"
        }
        ureq::Error::BodyExceedsLimit(_) => "it was bigger than it should be",
        ureq::Error::BodyStalled => return InstallFailure::Offline,
        ureq::Error::Protocol(_) | ureq::Error::LargeResponseHeader(..) => {
            "its site's answer couldn't be read"
        }
        _ => "something went wrong",
    };
    other(format!("the download failed: {why}"))
}

/// Unpacks the checked download `file` into `build`, then removes it.
/// Stops between entries once `stopped` is set.
fn unpack_into(
    build: &Path,
    file: &Path,
    unpack: Unpack,
    url: &str,
    stopped: &AtomicBool,
    pause: Duration,
) -> Result<(), InstallFailure> {
    let unpacked = match unpack {
        Unpack::No => {
            let name = program_name(url);
            let program = build.join(name);
            fs::rename(file, &program)
                .and_then(|()| fs::set_permissions(&program, fs::Permissions::from_mode(0o755)))
                .map_err(|error| io_failure(&error, false))?;
            return Ok(());
        }
        Unpack::Zip => unzip(file, Placing::new(build, stopped, pause)?),
        Unpack::TarGz => untar(file, Placing::new(build, stopped, pause)?),
        // No other kind is given yet.
        _ => Err(other("CAF doesn't know how to unpack this".into())),
    };
    fs::remove_file(file).map_err(|error| io_failure(&error, false))?;
    unpacked
}

/// The name a download that isn't unpacked is saved as: the last part of
/// its URL's path, or `program` if that's no good.
fn program_name(url: &str) -> OsString {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let after_host = path.split_once("://").map_or(path, |(_, rest)| rest);
    let last = after_host
        .split_once('/')
        .and_then(|(_, path)| path.rsplit('/').next())
        .unwrap_or_default();
    if last.is_empty() || last.starts_with('.') || last.contains('\\') {
        OsString::from("program")
    } else {
        OsString::from(last)
    }
}

/// Where an archive's entries go: inside `build`, and nowhere else.
///
/// Folders and files are made as they come, each only once its folder is
/// found, by following any links, to be inside the build folder. Links
/// are made last, once every file is written, so nothing is ever written
/// through one, and each must then lead, followed all the way, to
/// something inside the build folder: a chain of links each harmless on
/// its own can't climb out. Hard links must name a file already placed.
struct Placing<'a> {
    build: PathBuf,
    /// `build`, with every link in its path followed.
    real: PathBuf,
    links: Vec<(PathBuf, PathBuf)>,
    /// Set once the install is stopped: looked at between entries.
    stopped: &'a AtomicBool,
    /// How long syncing each file takes, as well: only tests make it
    /// longer.
    pause: Duration,
}

/// Why an archive is refused, in CAF's words: what's in it, never where
/// on this machine it was being put.
fn refused(why: &str) -> InstallFailure {
    other(format!("the download can't be unpacked: {why}"))
}

impl<'a> Placing<'a> {
    fn new(build: &Path, stopped: &'a AtomicBool, pause: Duration) -> Result<Self, InstallFailure> {
        let real = fs::canonicalize(build).map_err(|error| io_failure(&error, false))?;
        Ok(Self {
            build: build.to_owned(),
            real,
            links: Vec::new(),
            stopped,
            pause,
        })
    }

    /// Ends the unpacking if the install is stopped.
    fn go_on(&self) -> Result<(), InstallFailure> {
        if self.stopped.load(Ordering::Relaxed) {
            return Err(InstallFailure::Stopped);
        }
        Ok(())
    }

    /// Where `name`, an entry's name, goes: refused unless it's a path
    /// down from the archive's top.
    fn path(&self, name: &Path) -> Result<PathBuf, InstallFailure> {
        let plain = name
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
        if !plain || name.as_os_str().is_empty() {
            return Err(refused(&format!("{} is outside it", name.display())));
        }
        Ok(self.build.join(name))
    }

    /// Makes the folder `path` goes in, and checks it's inside.
    fn folder_for(&self, path: &Path, name: &Path) -> Result<(), InstallFailure> {
        self.make_folder(path.parent().unwrap_or(&self.build), name)
    }

    /// Makes `folder`, and what leads to it, all inside: the deepest part
    /// of its path already there is checked first, followed through any
    /// link, so no folder is ever made outside, empty or not.
    fn make_folder(&self, folder: &Path, name: &Path) -> Result<(), InstallFailure> {
        let there = folder
            .ancestors()
            .find(|ancestor| ancestor.exists())
            .unwrap_or(&self.build);
        self.inside(there, name)?;
        fs::create_dir_all(folder).map_err(|error| io_failure(&error, false))?;
        self.inside(folder, name)
    }

    /// Checks `path`, followed all the way, is inside the build folder.
    fn inside(&self, path: &Path, name: &Path) -> Result<(), InstallFailure> {
        match fs::canonicalize(path) {
            Ok(real) if real.starts_with(&self.real) => Ok(()),
            Ok(_) => Err(refused(&format!("{} leads outside it", name.display()))),
            Err(_) => Err(refused(&format!(
                "{} leads to nothing in it",
                name.display()
            ))),
        }
    }

    fn folder(&self, name: &Path) -> Result<(), InstallFailure> {
        let path = self.path(name)?;
        self.make_folder(&path, name)
    }

    /// Writes a file, with `mode`'s permissions alone.
    fn file(&self, name: &Path, mode: u32, mut from: impl Read) -> Result<(), InstallFailure> {
        let path = self.path(name)?;
        self.folder_for(&path, name)?;
        let mode = mode & 0o777;
        // New, so nothing already there, a link least of all, is written
        // through.
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
            .map_err(|error| io_failure(&error, false))?;
        io::copy(&mut from, &mut out).map_err(|error| io_failure(&error, false))?;
        // Synced while it's open: its mode may not let it be opened again.
        out.sync_all().map_err(|error| io_failure(&error, false))?;
        if !self.pause.is_zero() {
            std::thread::sleep(self.pause);
        }
        // The mode given when making it is cut by the umask.
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))
            .map_err(|error| io_failure(&error, false))
    }

    /// Notes a link, made once every file is.
    fn link(&mut self, name: &Path, target: &Path) -> Result<(), InstallFailure> {
        self.path(name)?;
        if !link_stays_inside(name, target) {
            return Err(refused(&format!("{} links outside it", name.display())));
        }
        self.links.push((name.to_owned(), target.to_owned()));
        Ok(())
    }

    /// Makes a hard link to a file already placed.
    fn hard_link(&self, name: &Path, target: &Path) -> Result<(), InstallFailure> {
        let path = self.path(name)?;
        let original = self.path(target)?;
        self.inside(&original, target)?;
        self.folder_for(&path, name)?;
        fs::hard_link(&original, &path).map_err(|error| io_failure(&error, false))
    }

    /// Makes the links, then checks each, followed all the way.
    fn finish(self) -> Result<(), InstallFailure> {
        for (name, target) in &self.links {
            let path = self.build.join(name);
            self.folder_for(&path, name)?;
            std::os::unix::fs::symlink(target, &path).map_err(|error| io_failure(&error, false))?;
        }
        for (name, _) in &self.links {
            self.inside(&self.build.join(name), name)?;
        }
        Ok(())
    }
}

/// Unpacks a zip archive, keeping each entry inside `build`.
fn unzip(file: &Path, mut placing: Placing<'_>) -> Result<(), InstallFailure> {
    let bad = |error: &dyn std::fmt::Display| refused(&error.to_string());
    let archive = File::open(file).map_err(|error| io_failure(&error, false))?;
    let mut archive = zip::ZipArchive::new(archive).map_err(|error| bad(&error))?;
    for index in 0..archive.len() {
        placing.go_on()?;
        let mut entry = archive.by_index(index).map_err(|error| bad(&error))?;
        let Some(name) = entry.enclosed_name() else {
            return Err(refused(&format!("{:?} is outside it", entry.name())));
        };
        if entry.is_dir() {
            placing.folder(&name)?;
        } else if entry.is_symlink() {
            let mut target = String::new();
            entry
                .read_to_string(&mut target)
                .map_err(|error| bad(&error))?;
            placing.link(&name, Path::new(&target))?;
        } else {
            let mode = entry.unix_mode().unwrap_or(0o644);
            placing.file(&name, mode, &mut entry)?;
        }
    }
    placing.finish()
}

/// Whether a link at `at` in an archive, to `target`, stays inside it, as
/// text: a first check, before the links are made and followed.
fn link_stays_inside(at: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    // How deep the link's folder is, then each part of the target.
    let mut depth = at.components().count().saturating_sub(1);
    for part in target.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                let Some(up) = depth.checked_sub(1) else {
                    return false;
                };
                depth = up;
            }
            _ => return false,
        }
    }
    true
}

/// Unpacks a gzipped tar archive, keeping each entry inside `build`, as
/// [`Placing`] does. Only folders, files and links are taken: a device or
/// a pipe is refused.
fn untar(file: &Path, mut placing: Placing<'_>) -> Result<(), InstallFailure> {
    let bad = |error: &io::Error| match error.raw_os_error() {
        Some(libc::ENOSPC | libc::EDQUOT) => InstallFailure::NoSpace,
        // The tar crate's own words, which name no folder of this machine's.
        _ => refused(&error.to_string()),
    };
    let archive = File::open(file).map_err(|error| io_failure(&error, false))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(io::BufReader::new(archive)));
    for entry in archive.entries().map_err(|error| bad(&error))? {
        placing.go_on()?;
        let mut entry = entry.map_err(|error| bad(&error))?;
        let name = entry.path().map_err(|error| bad(&error))?.into_owned();
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            placing.folder(&name)?;
        } else if kind.is_file() || kind == tar::EntryType::Continuous {
            let mode = entry.header().mode().unwrap_or(0o644);
            placing.file(&name, mode, &mut entry)?;
        } else if kind.is_symlink() || kind.is_hard_link() {
            let Some(target) = entry.link_name().map_err(|error| bad(&error))? else {
                return Err(refused(&format!("{} links to nothing", name.display())));
            };
            if kind.is_symlink() {
                placing.link(&name, &target)?;
            } else {
                placing.hard_link(&name, &target)?;
            }
        } else if kind.is_pax_global_extensions()
            || kind.is_pax_local_extensions()
            || kind.is_gnu_longname()
            || kind.is_gnu_longlink()
        {
            // Read by the tar crate for the entries after them.
        } else {
            return Err(refused(&format!(
                "{} isn't a file, folder or link",
                name.display()
            )));
        }
    }
    placing.finish()
}

/// How many bytes the files under `path` hold. Anything that can't be read
/// counts nothing.
fn size_of(path: &Path) -> u64 {
    let mut total = 0;
    let mut folders = vec![path.to_owned()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file() {
                total += entry.metadata().map_or(0, |metadata| metadata.len());
            }
        }
    }
    total
}

/// The major version of Node on `PATH`, if there is one.
async fn node_version() -> Option<u32> {
    let node = on_path("node")?;
    let said = probe(&node, &["--version"]).await?;
    said.trim()
        .strip_prefix('v')?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Whether npm is on `PATH`, and runs.
async fn has_npm() -> bool {
    match on_path("npm") {
        Some(npm) => probe(&npm, &["--version"]).await.is_some(),
        None => false,
    }
}

/// What `program` prints with `args`, if it ends well within [`PROBE`].
async fn probe(program: &Path, args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env("PATH", absolute_only(&path_var()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    let output = tokio::time::timeout(PROBE, command.output())
        .await
        .ok()?
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The `PATH` this program was given.
pub(crate) fn path_var() -> OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

/// `name`, found in one of `PATH`'s absolute folders, if it's a file that
/// can be run there.
pub(crate) fn on_path(name: &str) -> Option<PathBuf> {
    let path = path_var();
    std::env::split_paths(&path)
        .filter(|folder| folder.is_absolute())
        .map(|folder| folder.join(name))
        .find(|candidate| {
            fs::metadata(candidate).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
}

/// Kills the process group `child` leads. Only while it hasn't been
/// waited for, so its number can't yet name anything else.
pub(crate) fn kill_group(child: &tokio::process::Child) {
    if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: `kill` takes two numbers and touches no memory.
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

/// Runs `work` on a blocking thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|_| Err(io::Error::other("the work panicked")))
}

/// Bytes as lower-case hex.
pub(super) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// An install failure, in CAF's words.
fn other(why: String) -> InstallFailure {
    InstallFailure::Other(why)
}

/// An install failure for an error on the disk.
fn failure(error: io::Error) -> InstallFailure {
    io_failure(&error, false)
}

/// An install failure for an I/O error, on the network if `network`.
fn io_failure(error: &io::Error, network: bool) -> InstallFailure {
    match error.raw_os_error() {
        Some(libc::ENOSPC | libc::EDQUOT) => InstallFailure::NoSpace,
        _ if network => InstallFailure::Offline,
        _ => other(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_names_are_plain_and_never_private() {
        for good in ["claude-0.98.0", "codex-2.2.2+clackatoo.2", "a"] {
            assert!(is_install_name(good), "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".lock",
            ".build-claude",
            "a/b",
            "/abs",
            "../up",
            "claude-private",
        ] {
            assert!(!is_install_name(bad), "{bad}");
        }
    }

    #[test]
    fn links_must_stay_inside_the_archive() {
        let inside = |at: &str, to: &str| link_stays_inside(Path::new(at), Path::new(to));
        assert!(inside("bin/run", "../lib/run"));
        assert!(inside("run", "lib/run"));
        assert!(inside("a/b/c", "../../d"));
        assert!(!inside("run", "../escape"));
        assert!(!inside("bin/run", "../../escape"));
        assert!(!inside("run", "/etc/passwd"));
    }

    #[test]
    fn a_program_downloaded_is_named_after_its_url() {
        assert_eq!(program_name("https://example.com/v1/agent"), "agent");
        assert_eq!(program_name("https://example.com/v1/agent?x=1#y"), "agent");
        assert_eq!(program_name("https://example.com/"), "program");
        assert_eq!(program_name("https://example.com"), "program");
        assert_eq!(program_name("https://example.com/.hidden"), "program");
    }

    #[test]
    fn a_download_goes_through_a_proxy_as_npms_would() {
        let with = |vars: &[(&str, &str)], url: &str| {
            proxy_for(url, |name| {
                (vars.iter())
                    .find(|(each, _)| *each == name)
                    .map(|(_, value)| (*value).to_owned())
            })
            .map(|(_, value)| value)
        };
        let named = |vars: &[(&str, &str)]| {
            proxy_for("https://dl.example.com/x", |name| {
                (vars.iter())
                    .find(|(each, _)| *each == name)
                    .map(|(_, value)| (*value).to_owned())
            })
        };
        // npm's own settings come first, and the variable is told.
        assert_eq!(
            named(&[
                ("HTTPS_PROXY", "http://env"),
                ("npm_config_https_proxy", "http://npm")
            ]),
            Some(("npm_config_https_proxy", "http://npm".to_owned()))
        );
        assert_eq!(
            named(&[
                ("HTTPS_PROXY", "http://env"),
                ("NPM_CONFIG_PROXY", "http://npm")
            ]),
            Some(("NPM_CONFIG_PROXY", "http://npm".to_owned()))
        );
        assert_eq!(
            named(&[
                ("HTTPS_PROXY", "http://env"),
                ("npm_config_noproxy", "example.com")
            ]),
            None
        );
        let https = "https://dl.example.com:443/agent.tar.gz";
        assert_eq!(with(&[], https), None);
        assert_eq!(
            with(&[("HTTPS_PROXY", "http://proxy:3128")], https).as_deref(),
            Some("http://proxy:3128")
        );
        // HTTPS_PROXY first, then its lower case, then HTTP_PROXY.
        assert_eq!(
            with(
                &[
                    ("https_proxy", "http://low"),
                    ("HTTP_PROXY", "http://plain")
                ],
                https
            )
            .as_deref(),
            Some("http://low")
        );
        assert_eq!(
            with(&[("HTTP_PROXY", "http://plain")], https).as_deref(),
            Some("http://plain")
        );
        // ALL_PROXY isn't npm's, and an http download uses HTTP_PROXY only.
        assert_eq!(with(&[("ALL_PROXY", "http://all")], https), None);
        assert_eq!(
            with(&[("HTTPS_PROXY", "http://proxy")], "http://127.0.0.1:9/x"),
            None
        );
        // NO_PROXY names the host, or a domain it's in.
        let proxy = ("HTTPS_PROXY", "http://proxy");
        for skip in [
            "dl.example.com",
            "example.com",
            ".example.com",
            "other.org, example.com",
            "COM",
        ] {
            assert_eq!(with(&[proxy, ("NO_PROXY", skip)], https), None, "{skip}");
        }
        for keep in ["ample.com", "x.dl.example.com", "example.org", ",", ""] {
            assert!(
                with(&[proxy, ("no_proxy", keep)], https).is_some(),
                "{keep}"
            );
        }
        assert_eq!(
            with(&[proxy, ("NO_PROXY", "user")], "https://me@user:8080/x"),
            None,
            "the host, not its user or port"
        );
    }

    #[tokio::test]
    async fn a_fetch_that_stalls_ends_by_itself() {
        // Half a body, then nothing, for good.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/x", listener.local_addr().unwrap());
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 1024];
            let _ = socket.read(&mut buffer).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nhalf")
                .await;
            std::future::pending::<()>().await;
        });
        let temp = std::env::temp_dir().join(format!("caf-fetch-{}", std::process::id()));
        let out = File::create(&temp).unwrap();
        let fetched = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::task::spawn_blocking(move || {
                let (done, stopped) = (AtomicU64::new(0), AtomicBool::new(false));
                let how = Fetch {
                    url,
                    proxy: None,
                    https_only: false,
                    idle: Duration::from_millis(500),
                    most: u64::MAX,
                };
                fetch(&how, out, &done, &stopped)
            }),
        )
        .await
        .expect("the fetch ends by itself")
        .unwrap();
        let _ = fs::remove_file(&temp);
        assert_eq!(fetched, Err(InstallFailure::Offline));
    }

    #[test]
    fn an_install_is_synced_whatever_it_holds() {
        let folder = std::env::temp_dir().join(format!("caf-sync-{}", std::process::id()));
        fs::create_dir_all(folder.join("bin/deep")).unwrap();
        fs::write(folder.join("bin/deep/agent"), "x").unwrap();
        std::os::unix::fs::symlink("missing", folder.join("link")).unwrap();
        let synced = sync_all_in(&folder, true, Duration::ZERO, &AtomicBool::new(false));
        let stopped = sync_all_in(&folder, true, Duration::ZERO, &AtomicBool::new(true));
        fs::remove_dir_all(&folder).unwrap();
        assert!(
            synced.expect("folders, files and links, even one leading nowhere"),
            "all synced"
        );
        assert!(!stopped.unwrap(), "stopped, it says so");
    }

    #[test]
    fn npm_says_only_its_code() {
        let said =
            "npm error code E404\nnpm error 404 Not Found - GET https://secret.example/token=abc\n";
        assert_eq!(
            npm_failure(said, Some(1)),
            InstallFailure::Other("npm ci exited with 1 (E404)".into())
        );
        assert_eq!(
            npm_failure("npm ERR! code ENOTFOUND", Some(1)),
            InstallFailure::Offline
        );
        assert_eq!(
            npm_failure("npm error code ENOSPC", Some(1)),
            InstallFailure::NoSpace
        );
        assert_eq!(
            npm_failure("npm error code EINTEGRITY", Some(1)),
            InstallFailure::Corrupt
        );
        assert_eq!(
            npm_failure("whatever", None),
            InstallFailure::Other("npm ci was killed".into())
        );
    }
}
