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
use rustix::fs::FlockOperation;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::sync::oneshot;

use super::{Options, Reporter, hidden_name};
use crate::agent::absolute_only;

/// How often a lock is tried while waiting for it, and a sign-in's
/// `BROWSER` folder looked in.
pub(crate) const POLL: Duration = Duration::from_millis(100);

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
            if let Some(lock) = try_lock(&root.join(LOCK), Lock::Exclusive)? {
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
    let (node, npm) = tokio::join!(node_version(), has_npm());
    reporter.last(SetupEvent::Surveyed {
        root,
        installed,
        node,
        npm,
        platform: Platform::current(),
    });
}

/// Installs `install` as `root/folder`.
pub(super) async fn install(
    root: PathBuf,
    folder: String,
    install: Install,
    options: Options,
    reporter: Reporter,
    mut stop: oneshot::Receiver<()>,
) {
    let outcome = installing(&root, &folder, install, &options, &reporter, &mut stop).await;
    reporter.last(match outcome {
        Ok(()) => SetupEvent::Installed,
        Err(failure) => SetupEvent::InstallFailed(failure),
    });
}

/// Installs, holding `.lock` throughout, and removes what it built if it
/// fails.
async fn installing(
    root: &Path,
    folder: &str,
    install: Install,
    options: &Options,
    reporter: &Reporter,
    stop: &mut oneshot::Receiver<()>,
) -> Result<(), InstallFailure> {
    if !is_install_name(folder) {
        return Err(other(format!("{folder:?} can't be an install's name")));
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
        blocking(move || {
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
    let built = match install {
        Install::Npm(package) => npm(&build, &package, options, reporter, stop).await,
        Install::Download(artifacts) => download(&build, &artifacts, options, reporter, stop).await,
        // No other kind is given yet.
        _ => Err(other("CAF doesn't know this kind of install".into())),
    };
    let done = match built {
        Ok(()) => {
            let (build, place) = (build.clone(), place.clone());
            blocking(move || fs::rename(&build, &place))
                .await
                .map_err(failure)
        }
        Err(failure) => Err(failure),
    };
    if done.is_err() {
        blocking(move || remove_all(&build)).await.ok();
    }
    drop(lock);
    done
}

/// Removes `root/folder`, unless something runs from it.
pub(super) async fn remove(root: PathBuf, folder: String, reporter: Reporter) {
    let removed = removing(&root, &folder).await;
    let removed = removed.unwrap_or_else(|error| {
        log::warn!(target: "caf", "can't remove {folder} from {}: {error}", root.display());
        false
    });
    reporter.last(SetupEvent::Removed { removed });
}

async fn removing(root: &Path, folder: &str) -> io::Result<bool> {
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
    let Some(npm) = on_path("npm") else {
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
    let said = tokio::spawn(async move {
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
    let said = said.await.unwrap_or_default();
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
    let done = Arc::new(AtomicU64::new(0));
    let stopped = Arc::new(AtomicBool::new(false));
    let fetching = {
        let (url, file, done, stopped) = (
            artifact.url.clone(),
            file.clone(),
            Arc::clone(&done),
            Arc::clone(&stopped),
        );
        let https_only = !allowed_http(options);
        tokio::task::spawn_blocking(move || fetch(&url, &file, https_only, &done, &stopped))
    };
    tokio::pin!(fetching);
    let mut ticks = tokio::time::interval(PROGRESS);
    let mut told = 0;
    let mut was_stopped = false;
    let fetched = loop {
        tokio::select! {
            fetched = &mut fetching => break fetched,
            _ = &mut *stop, if !was_stopped => {
                // The fetch sees it between reads, and ends.
                was_stopped = true;
                stopped.store(true, Ordering::Relaxed);
            }
            _ = ticks.tick() => {
                let now = done.load(Ordering::Relaxed);
                if now != told {
                    told = now;
                    reporter.send(SetupEvent::Progress { done: now });
                }
            }
        }
    };
    if was_stopped {
        return Err(InstallFailure::Stopped);
    }
    let sha256 = fetched.unwrap_or_else(|_| Err(other("the download panicked".into())))?;
    if !sha256.eq_ignore_ascii_case(artifact.sha256.trim()) {
        return Err(InstallFailure::Corrupt);
    }
    let (build, unpack, url) = (build.to_owned(), artifact.unpack, artifact.url.clone());
    tokio::task::spawn_blocking(move || unpack_into(&build, &file, unpack, &url))
        .await
        .unwrap_or_else(|_| Err(other("unpacking panicked".into())))
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

/// Downloads `url` to `file`, counting into `done`, until it's done or
/// `stopped`. Returns its SHA-256, in hex.
fn fetch(
    url: &str,
    file: &Path,
    https_only: bool,
    done: &AtomicU64,
    stopped: &AtomicBool,
) -> Result<String, InstallFailure> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(https_only)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .http_status_as_error(true)
        .build()
        .into();
    let response = agent.get(url).call().map_err(|error| match error {
        ureq::Error::StatusCode(code) => other(format!("the download answered HTTP {code}")),
        ureq::Error::Io(error) => io_failure(&error, true),
        ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Timeout(_)
        | ureq::Error::Tls(_)
        | ureq::Error::Rustls(_) => InstallFailure::Offline,
        error => other(format!("the download failed: {error}")),
    })?;
    let mut body = response.into_body();
    let mut reader = body.with_config().limit(u64::MAX).reader();
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(file)
        .map_err(|error| io_failure(&error, false))?;
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
        hash.update(&buffer[..n]);
        out.write_all(&buffer[..n])
            .map_err(|error| io_failure(&error, false))?;
        done.fetch_add(n as u64, Ordering::Relaxed);
    }
    out.sync_all().map_err(|error| io_failure(&error, false))?;
    Ok(hex(&hash.finalize()))
}

/// Unpacks the checked download `file` into `build`, then removes it.
fn unpack_into(build: &Path, file: &Path, unpack: Unpack, url: &str) -> Result<(), InstallFailure> {
    let unpacked = match unpack {
        Unpack::No => {
            let name = program_name(url);
            let program = build.join(name);
            fs::rename(file, &program)
                .and_then(|()| fs::set_permissions(&program, fs::Permissions::from_mode(0o755)))
                .map_err(|error| io_failure(&error, false))?;
            return Ok(());
        }
        Unpack::Zip => unzip(build, file),
        Unpack::TarGz => untar(build, file),
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

/// Unpacks a zip archive, keeping each entry inside `build`.
fn unzip(build: &Path, file: &Path) -> Result<(), InstallFailure> {
    let bad =
        |error: &dyn std::fmt::Display| other(format!("the download can't be unzipped: {error}"));
    let archive = File::open(file).map_err(|error| io_failure(&error, false))?;
    let mut archive = zip::ZipArchive::new(archive).map_err(|error| bad(&error))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| bad(&error))?;
        let Some(name) = entry.enclosed_name() else {
            return Err(bad(&format!("{:?} is outside the archive", entry.name())));
        };
        let path = build.join(&name);
        if entry.is_dir() {
            fs::create_dir_all(&path).map_err(|error| io_failure(&error, false))?;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| io_failure(&error, false))?;
        }
        if entry.is_symlink() {
            let mut target = String::new();
            entry
                .read_to_string(&mut target)
                .map_err(|error| bad(&error))?;
            if !link_stays_inside(&name, Path::new(&target)) {
                return Err(bad(&format!("{name:?} links outside the archive")));
            }
            std::os::unix::fs::symlink(&target, &path)
                .map_err(|error| io_failure(&error, false))?;
            continue;
        }
        let mode = entry.unix_mode().map_or(0o644, |mode| mode & 0o777);
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
            .map_err(|error| io_failure(&error, false))?;
        io::copy(&mut entry, &mut out).map_err(|error| io_failure(&error, false))?;
        // The mode given when making it is cut by the umask.
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))
            .map_err(|error| io_failure(&error, false))?;
    }
    Ok(())
}

/// Whether a link at `at` in an archive, to `target`, stays inside it.
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

/// Unpacks a gzipped tar archive. The tar crate keeps every entry, link
/// targets included, inside `build`.
fn untar(build: &Path, file: &Path) -> Result<(), InstallFailure> {
    let archive = File::open(file).map_err(|error| io_failure(&error, false))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(io::BufReader::new(archive)));
    archive.set_preserve_permissions(false);
    archive.set_overwrite(false);
    archive
        .unpack(build)
        .map_err(|error| match error.raw_os_error() {
            Some(libc::ENOSPC) => InstallFailure::NoSpace,
            _ => other(format!("the download can't be untarred: {error}")),
        })
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
