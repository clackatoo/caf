//! The setup driver, offline: npm from a local package, downloads from a
//! local server, removals while a process runs, and sign-ins with the
//! fake agent's `auth` commands.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use caf_core::agent::{EnvChange, InstallFailure, SpawnCommand};
use caf_core::directive::{AgentDirective, SetupDirective, Signal};
use caf_core::event::{AgentEvent, SetupEvent};
use caf_core::ids::Ticket;
use caf_core::install::{Artifact, Install, LinkFrom, NpmPackage, Platform, SignedInAs, Unpack};
use caf_fake_agent::{AUTH_EMAIL, AUTH_SECRET, AUTH_VAR, Auth};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

use super::{Options, SetupDriver};
use crate::{AgentDriver, AgentLimits};

/// Long enough for anything a test waits for, on a loaded machine.
const PATIENCE: Duration = Duration::from_secs(60);

/// A folder of its own for each test, under `TMPDIR`.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(1000);
        let dir = std::env::temp_dir().join(format!(
            "caf-setup-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Where installs go: made by the driver.
    fn root(&self) -> PathBuf {
        self.path("agents")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A driver for tests: `http` to this machine allowed, and npm offline,
/// with a cache of the test's own.
fn driver(dir: &Dir) -> SetupDriver {
    std::fs::create_dir_all(dir.path("temp")).unwrap();
    SetupDriver::with_options(
        dir.root(),
        Options {
            http_to_this_machine: true,
            // Not the cache the package was made with, which holds it.
            npm_offline_cache: Some(dir.path("driver-npm-cache")),
            temp: Some(dir.path("temp")),
            env: Some(std::collections::HashMap::new()),
            idle: None,
            npm: None,
        },
    )
}

/// A report, compared by what it says: `SetupEvent` has no `==` of its
/// own.
struct Ev(SetupEvent);

impl std::fmt::Debug for Ev {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq<SetupEvent> for Ev {
    fn eq(&self, other: &SetupEvent) -> bool {
        format!("{:?}", self.0) == format!("{other:?}")
    }
}

/// Whether `event` is a job's last.
fn is_last(event: &SetupEvent) -> bool {
    !matches!(
        event,
        SetupEvent::Progress { .. } | SetupEvent::Link(_) | SetupEvent::Code(_)
    )
}

/// What the driver reports about `ticket`, up to its last, which comes
/// last in the list.
async fn done(driver: &mut SetupDriver, ticket: Ticket) -> Vec<Ev> {
    let mut events = Vec::new();
    tokio::time::timeout(PATIENCE, async {
        loop {
            let (about, event) = driver.next().await;
            assert_eq!(about, ticket, "a report about another job: {event:?}");
            let last = is_last(&event);
            events.push(Ev(event));
            if last {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the job ended: {events:?}"));
    assert!(
        driver.jobs.is_empty(),
        "the job is forgotten once it's done"
    );
    events
}

/// The next report about `ticket`, which mustn't be its last.
async fn next_of(driver: &mut SetupDriver, ticket: Ticket) -> Ev {
    let (about, event) = tokio::time::timeout(PATIENCE, driver.next())
        .await
        .expect("a report");
    assert_eq!(about, ticket);
    Ev(event)
}

/// The names in `folder`, sorted.
fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// What's in the root, besides its lock: only what's installed.
fn installs(dir: &Dir) -> Vec<String> {
    let mut names = names(&dir.root());
    names.retain(|name| name != ".lock");
    names
}

#[tokio::test]
async fn a_survey_says_what_is_installed_and_cleans_up_what_was_cut_off() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Survey {
        ticket: Ticket(1000),
    });
    let events = done(&mut driver, Ticket(1000)).await;
    let [
        Ev(SetupEvent::Surveyed {
            root,
            installed,
            node,
            npm,
            platform,
            used,
        }),
    ] = events.as_slice()
    else {
        panic!("{events:?}")
    };
    assert_eq!(root, &dir.root());
    assert!(installed.is_empty() && used.is_empty());
    assert_eq!(*platform, Platform::current());
    assert!(!dir.root().exists(), "a survey makes nothing");
    // This machine's Node and npm, as the tests need them.
    assert!(node.is_some_and(|node| node >= 18), "{node:?}");
    assert!(npm);

    for folder in [
        "claude-0.98.0",
        "codex-2.2.2+clackatoo.2",
        "claude-private",
        ".build-claude-0.99.0-1-1",
        ".trash-claude-0.97.0-1-1",
    ] {
        std::fs::create_dir_all(dir.root().join(folder)).unwrap();
        std::fs::write(dir.root().join(folder).join("file"), "x").unwrap();
    }
    std::fs::write(dir.root().join("stray-file"), "").unwrap();
    // While something holds the lock, the hidden folders may be in use.
    let held = std::fs::File::create(dir.root().join(".lock")).unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).unwrap();
    driver.apply(SetupDirective::Survey {
        ticket: Ticket(1001),
    });
    let events = done(&mut driver, Ticket(1001)).await;
    let [Ev(SetupEvent::Surveyed { installed, .. })] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(installed, &["claude-0.98.0", "codex-2.2.2+clackatoo.2"]);
    assert!(dir.root().join(".build-claude-0.99.0-1-1").exists());
    drop(held);
    driver.apply(SetupDirective::Survey {
        ticket: Ticket(1002),
    });
    done(&mut driver, Ticket(1002)).await;
    assert_eq!(
        installs(&dir),
        [
            "claude-0.98.0",
            "claude-private",
            "codex-2.2.2+clackatoo.2",
            "stray-file"
        ],
        "only what was cut off is removed"
    );
}

#[tokio::test]
async fn a_survey_that_cant_take_the_lock_still_says_whats_installed() {
    let dir = Dir::new();
    std::fs::create_dir_all(dir.root().join("fake-1.0.0")).unwrap();
    // A root that can't be written to, so its lock can't be made.
    std::fs::set_permissions(dir.root(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Survey {
        ticket: Ticket(1000),
    });
    let events = done(&mut driver, Ticket(1000)).await;
    std::fs::set_permissions(dir.root(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let [Ev(SetupEvent::Surveyed { installed, .. })] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(installed, &["fake-1.0.0"]);
}

/// A package `fake-adapter` 1.0.0 with a program of the same name, packed
/// in `dir`, and the manifest and lockfile that install it, offline.
fn npm_package(dir: &Dir) -> NpmPackage {
    let package = dir.path("package");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"name": "fake-adapter", "version": "1.0.0", "bin": {"fake-adapter": "bin.js"}, "scripts": {"install": "touch ../../installed-by-script"}}"#,
    )
    .unwrap();
    std::fs::write(
        package.join("bin.js"),
        "#!/usr/bin/env node\nconsole.log('hi');\n",
    )
    .unwrap();
    let cache = dir.path("npm-cache");
    let npm = |args: &[&str], cwd: &Path| {
        let output = Command::new("npm")
            .args(args)
            .args(["--offline", "--ignore-scripts", "--no-audit", "--no-fund"])
            .arg("--cache")
            .arg(&cache)
            .current_dir(cwd)
            .output()
            .expect("npm runs");
        assert!(output.status.success(), "{output:?}");
    };
    npm(&["pack"], &package);
    let tarball = package.join("fake-adapter-1.0.0.tgz");
    let lock = dir.path("lock");
    std::fs::create_dir_all(&lock).unwrap();
    let manifest = format!(
        r#"{{"name": "caf-install", "version": "0.0.0", "private": true, "dependencies": {{"fake-adapter": "file:{}"}}}}"#,
        tarball.display()
    );
    std::fs::write(lock.join("package.json"), &manifest).unwrap();
    npm(&["install", "--package-lock-only"], &lock);
    let lockfile = std::fs::read_to_string(lock.join("package-lock.json")).unwrap();
    // npm writes where the package is from relative to the lockfile: it's
    // installed elsewhere.
    let lockfile = lockfile.replace("file:../package/", &format!("file:{}/", package.display()));
    NpmPackage::new(manifest, lockfile)
}

#[tokio::test]
async fn npm_installs_from_the_lockfile_into_place() {
    let dir = Dir::new();
    let package = npm_package(&dir);
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1000),
        folder: "fake-1.0.0".into(),
        install: Install::Npm(package),
        private: None,
        size: 1 << 30,
    });
    let events = done(&mut driver, Ticket(1000)).await;
    assert!(
        matches!(events.last(), Some(Ev(SetupEvent::Installed))),
        "{events:?}"
    );
    assert_eq!(
        installs(&dir),
        ["fake-1.0.0"],
        "built in a hidden folder, then renamed"
    );
    let mode = std::fs::metadata(dir.root()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700, "the root is the user's alone");
    let program = dir.root().join("fake-1.0.0/node_modules/.bin/fake-adapter");
    assert!(program.exists(), "the package's program is linked");
    assert!(
        // npm runs a package's install script in its folder,
        // node_modules/fake-adapter: two up is the install itself.
        !dir.root().join("fake-1.0.0/installed-by-script").exists(),
        "no install script runs"
    );
}

#[tokio::test]
async fn stopping_an_npm_install_stops_all_it_started() {
    let dir = Dir::new();
    // An npm that starts something and waits on it, as npm does.
    let pid_file = dir.path("started.pid");
    let npm = dir.path("npm");
    std::fs::write(
        &npm,
        format!(
            "#!/bin/sh\nsleep 300 &\necho $! > {}.new && mv {0}.new {0}\nwait\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut driver = SetupDriver::with_options(
        dir.root(),
        Options {
            npm: Some(npm),
            env: Some(std::collections::HashMap::new()),
            ..Options::default()
        },
    );
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1000),
        folder: "fake-1.0.0".into(),
        install: Install::Npm(NpmPackage::new("{}", "{}")),
        private: None,
        size: 1 << 30,
    });
    let started = tokio::time::timeout(PATIENCE, async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file) {
                return pid.trim().parse::<i32>().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("npm started");
    driver.apply(SetupDirective::Stop {
        ticket: Ticket(1000),
    });
    let events = tokio::time::timeout(Duration::from_secs(3), done(&mut driver, Ticket(1000)))
        .await
        .expect("stopped within a second or so");
    assert_eq!(
        events.last().expect("a last report"),
        &SetupEvent::InstallFailed(InstallFailure::Stopped)
    );
    assert!(installs(&dir).is_empty(), "{:?}", installs(&dir));
    let gone = tokio::time::timeout(Duration::from_secs(5), async {
        // SAFETY: signal 0 sends nothing.
        #[allow(unsafe_code)]
        while unsafe { libc::kill(started, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(gone.is_ok(), "what npm started was stopped with it");
}

#[tokio::test]
async fn a_failed_npm_install_leaves_nothing() {
    let dir = Dir::new();
    let package = npm_package(&dir);
    std::fs::remove_file(dir.path("package/fake-adapter-1.0.0.tgz")).unwrap();
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1000),
        folder: "fake-1.0.0".into(),
        install: Install::Npm(package),
        private: None,
        size: 1 << 30,
    });
    let events = done(&mut driver, Ticket(1000)).await;
    assert!(
        matches!(events.last(), Some(Ev(SetupEvent::InstallFailed(_)))),
        "{events:?}"
    );
    assert!(installs(&dir).is_empty(), "{:?}", installs(&dir));
}

#[tokio::test]
async fn a_package_that_doesnt_match_its_lockfile_is_thrown_away() {
    let dir = Dir::new();
    let package = npm_package(&dir);
    std::fs::write(
        dir.path("package/fake-adapter-1.0.0.tgz"),
        b"not what was locked",
    )
    .unwrap();
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1000),
        folder: "fake-1.0.0".into(),
        install: Install::Npm(package),
        private: None,
        size: 1 << 30,
    });
    let events = done(&mut driver, Ticket(1000)).await;
    assert_eq!(
        events.last().expect("a last report"),
        &SetupEvent::InstallFailed(InstallFailure::Corrupt),
        "{events:?}"
    );
    assert!(installs(&dir).is_empty(), "{:?}", installs(&dir));
}

/// A server on this machine, giving `body` to every request, counting
/// them. If `stall`, it sends half and waits until let go.
struct Server {
    url: String,
    requests: Arc<AtomicUsize>,
    go: Arc<Notify>,
}

async fn serve(body: Vec<u8>, stall: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/fake-adapter", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let go = Arc::new(Notify::new());
    let body = Arc::new(body);
    {
        let (requests, go) = (Arc::clone(&requests), Arc::clone(&go));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (requests, go, body) =
                    (Arc::clone(&requests), Arc::clone(&go), Arc::clone(&body));
                tokio::spawn(async move {
                    let mut asked = Vec::new();
                    let mut buffer = [0; 1024];
                    while !asked.windows(4).any(|end| end == b"\r\n\r\n") {
                        match socket.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => asked.extend_from_slice(&buffer[..n]),
                        }
                    }
                    requests.fetch_add(1, Ordering::Relaxed);
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let half = if stall { body.len() / 2 } else { body.len() };
                    let _ = socket.write_all(&body[..half]).await;
                    if stall {
                        go.notified().await;
                    }
                    let _ = socket.write_all(&body[half..]).await;
                });
            }
        });
    }
    Server { url, requests, go }
}

fn sha256(bytes: &[u8]) -> String {
    super::install::hex(&Sha256::digest(bytes))
}

/// A gzipped tar of a runnable `bin/agent` and a `README`.
fn tar_gz() -> Vec<u8> {
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    for (path, mode, text) in [
        ("bin/agent", 0o755, &b"#!/bin/sh\necho agent\n"[..]),
        ("README", 0o644, b"read me"),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(text.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        archive.append_data(&mut header, path, text).unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

/// A zip of a runnable `bin/agent`, a link to it, and a `README`.
fn zip() -> Vec<u8> {
    use std::io::Write as _;
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = |mode| {
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(mode)
    };
    archive.start_file("bin/agent", options(0o755)).unwrap();
    archive.write_all(b"#!/bin/sh\necho agent\n").unwrap();
    archive.start_file("README", options(0o644)).unwrap();
    archive.write_all(b"read me").unwrap();
    archive
        .add_symlink("agent", "bin/agent", options(0o777))
        .unwrap();
    archive.finish().unwrap().into_inner()
}

/// What `program` prints, run as soon as it can be: just written, it may
/// be "text file busy" for a moment, while a process another test is
/// starting holds it open for writing.
fn run(program: &Path) -> std::process::Output {
    for _ in 0..200 {
        match Command::new(program).output() {
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            output => return output.unwrap(),
        }
    }
    panic!("{} stays busy", program.display())
}

fn download(url: &str, body: &[u8], unpack: Unpack) -> Install {
    Install::Download(vec![
        Artifact::new(
            Platform::new("plan9", "mips"),
            "https://example.com/wrong-one",
            "00",
            Unpack::No,
        ),
        Artifact::new(Platform::current(), url, sha256(body), unpack),
    ])
}

#[tokio::test]
async fn downloads_are_checked_and_unpacked_into_place() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    for (n, (body, unpack)) in [
        (tar_gz(), Unpack::TarGz),
        (zip(), Unpack::Zip),
        (b"#!/bin/sh\necho agent\n".to_vec(), Unpack::No),
    ]
    .into_iter()
    .enumerate()
    {
        let server = serve(body.clone(), false).await;
        let ticket = Ticket(1000 + n as u64);
        let folder = format!("fake-{n}");
        driver.apply(SetupDirective::Install {
            ticket,
            folder: folder.clone(),
            install: download(&server.url, &body, unpack),
            private: None,
            size: 1 << 30,
        });
        let events = done(&mut driver, ticket).await;
        assert!(
            matches!(events.last(), Some(Ev(SetupEvent::Installed))),
            "{events:?}"
        );
        let install = dir.root().join(&folder);
        let program = match unpack {
            Unpack::No => install.join("fake-adapter"),
            _ => install.join("bin/agent"),
        };
        let mode = std::fs::metadata(&program).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "{unpack:?}: it can be run");
        let output = run(&program);
        assert_eq!(output.stdout, b"agent\n");
        if unpack == Unpack::Zip {
            assert_eq!(
                std::fs::read_link(install.join("agent")).unwrap(),
                Path::new("bin/agent")
            );
        }
        assert!(
            !install.join(".download").exists(),
            "only what was unpacked"
        );
    }
    assert_eq!(installs(&dir), ["fake-0", "fake-1", "fake-2"]);
}

/// A zip of these entries, in order: a link is `(name, Some(target))`,
/// a file `(name, None)`.
fn zip_of(entries: &[(&str, Option<&str>)]) -> Vec<u8> {
    use std::io::Write as _;
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
    for (name, target) in entries {
        match target {
            Some(target) => archive.add_symlink(*name, *target, options).unwrap(),
            None => {
                archive.start_file(*name, options).unwrap();
                archive.write_all(b"escaped").unwrap();
            }
        }
    }
    archive.finish().unwrap().into_inner()
}

/// A gzipped tar of these entries, as [`zip_of`]'s.
fn tar_of(entries: &[(&str, Option<&str>)]) -> Vec<u8> {
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    for (name, target) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        match target {
            Some(target) => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                archive.append_link(&mut header, *name, *target).unwrap();
            }
            None => {
                header.set_size(7);
                header.set_cksum();
                archive
                    .append_data(&mut header, *name, &b"escaped"[..])
                    .unwrap();
            }
        }
    }
    archive.into_inner().unwrap().finish().unwrap()
}

#[tokio::test]
async fn nothing_in_a_download_reaches_outside_its_install() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    // Each link's target, read as text, stays inside; followed, the chain
    // climbs out, and files written through it would land above.
    let chain: &[(&str, Option<&str>)] = &[
        ("a/l1", Some("..")),
        ("l2", Some("a/l1/..")),
        ("l3", Some("l2/..")),
        ("l2/escaped-into-root", None),
        ("l3/escaped-above-root", None),
    ];
    let links_out: &[(&str, Option<&str>)] = &[("a/l1", Some("..")), ("l2", Some("a/l1/.."))];
    let absolute: &[(&str, Option<&str>)] = &[("passwd", Some("/etc/passwd"))];
    let up: &[(&str, Option<&str>)] = &[("up", Some("../../outside"))];
    let nowhere: &[(&str, Option<&str>)] = &[("gone", Some("missing/file"))];
    let mut n = 0;
    for entries in [chain, links_out, absolute, up, nowhere] {
        for (body, unpack) in [
            (zip_of(entries), Unpack::Zip),
            (tar_of(entries), Unpack::TarGz),
        ] {
            n += 1;
            let server = serve(body.clone(), false).await;
            let ticket = Ticket(1000 + n);
            driver.apply(SetupDirective::Install {
                ticket,
                folder: "fake-1.0.0".into(),
                install: download(&server.url, &body, unpack),
                private: None,
                size: 1 << 30,
            });
            let events = done(&mut driver, ticket).await;
            let what = format!("{unpack:?} of {entries:?}");
            let Some(Ev(SetupEvent::InstallFailed(InstallFailure::Other(why)))) = events.last()
            else {
                panic!("{what} is refused: {events:?}")
            };
            assert!(
                !why.contains(&*dir.0.to_string_lossy()),
                "{what}: no folder of the user's is told: {why}"
            );
            assert!(installs(&dir).is_empty(), "{what}: {:?}", installs(&dir));
            for escaped in [
                dir.root().join("escaped-into-root"),
                dir.0.join("escaped-above-root"),
            ] {
                assert!(
                    !escaped.exists(),
                    "{what}: {} was written",
                    escaped.display()
                );
            }
        }
    }
}

#[tokio::test]
async fn a_download_far_bigger_than_its_agent_is_cut_off() {
    let dir = Dir::new();
    let body = vec![7; 4 << 20];
    let server = serve(body.clone(), false).await;
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1000),
        folder: "fake-1.0.0".into(),
        install: download(&server.url, &body, Unpack::No),
        private: None,
        // Its agent takes 1,000 bytes: 4 MiB is far past that.
        size: 1000,
    });
    let events = done(&mut driver, Ticket(1000)).await;
    assert!(
        matches!(events.last(), Some(Ev(SetupEvent::InstallFailed(InstallFailure::Other(why)))) if why.contains("bigger")),
        "{events:?}"
    );
    assert!(installs(&dir).is_empty(), "{:?}", installs(&dir));
}

#[tokio::test]
async fn a_download_that_doesnt_match_its_checksum_is_thrown_away() {
    let dir = Dir::new();
    let server = serve(tar_gz(), false).await;
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1000),
        folder: "fake-1.0.0".into(),
        install: download(&server.url, b"something else", Unpack::TarGz),
        private: None,
        size: 1 << 30,
    });
    let events = done(&mut driver, Ticket(1000)).await;
    assert_eq!(
        events.last().expect("a last report"),
        &(SetupEvent::InstallFailed(InstallFailure::Corrupt)),
        "{events:?}"
    );
    assert!(installs(&dir).is_empty(), "{:?}", installs(&dir));
}

#[tokio::test]
async fn downloads_must_be_https() {
    let dir = Dir::new();
    let body = tar_gz();
    let server = serve(body.clone(), false).await;
    // Without the tests' allowance, as every driver is.
    let mut driver = SetupDriver::new(dir.root());
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1000),
        folder: "fake-1.0.0".into(),
        install: download(&server.url, &body, Unpack::TarGz),
        private: None,
        size: 1 << 30,
    });
    let events = done(&mut driver, Ticket(1000)).await;
    assert!(
        matches!(events.last(), Some(Ev(SetupEvent::InstallFailed(InstallFailure::Other(why)))) if why.contains("https")),
        "{events:?}"
    );
    assert_eq!(
        server.requests.load(Ordering::Relaxed),
        0,
        "nothing was asked for"
    );
}

#[tokio::test]
async fn an_install_stopped_partway_leaves_nothing() {
    let dir = Dir::new();
    let body = vec![7; 4 << 20];
    let server = serve(body.clone(), true).await;
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1000),
        folder: "fake-1.0.0".into(),
        install: download(&server.url, &body, Unpack::No),
        private: None,
        size: 1 << 30,
    });
    // Half of it comes, and is told.
    loop {
        match next_of(&mut driver, Ticket(1000)).await {
            Ev(SetupEvent::Progress { done }) if done >= 2 << 20 => break,
            Ev(SetupEvent::Progress { .. }) => {}
            other => panic!("{other:?}"),
        }
    }
    let building = installs(&dir);
    assert!(
        building.len() == 1 && building[0].starts_with(".build-fake-1.0.0-"),
        "{building:?}"
    );
    driver.apply(SetupDirective::Stop {
        ticket: Ticket(1000),
    });
    // The rest comes, so the download sees it's stopped.
    server.go.notify_waiters();
    let events = done(&mut driver, Ticket(1000)).await;
    assert_eq!(
        events.last().expect("a last report"),
        &(SetupEvent::InstallFailed(InstallFailure::Stopped))
    );
    assert!(installs(&dir).is_empty(), "{:?}", installs(&dir));
}

/// A server on this machine that takes requests and never answers.
async fn silent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/fake-adapter", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    url
}

#[tokio::test]
async fn a_download_goes_through_the_proxy_the_environment_names() {
    let dir = Dir::new();
    let body = tar_gz();
    let (proxy_url, tunnels) = proxy(body.clone()).await;
    // Nothing listens here: only the proxy can fetch it.
    let url = "http://127.0.0.1:9/v1/fake-adapter";
    for (n, (no_proxy, through)) in [("", true), ("127.0.0.1", false)].into_iter().enumerate() {
        let env = [("HTTP_PROXY", proxy_url.as_str()), ("NO_PROXY", no_proxy)]
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        let mut driver = SetupDriver::with_options(
            dir.root(),
            Options {
                http_to_this_machine: true,
                env: Some(env),
                ..Options::default()
            },
        );
        let ticket = Ticket(1000 + n as u64);
        driver.apply(SetupDirective::Install {
            ticket,
            folder: format!("fake-{n}"),
            install: download(url, &body, Unpack::TarGz),
            private: None,
            size: 1 << 30,
        });
        let events = done(&mut driver, ticket).await;
        let last = events.last().expect("a last report");
        if through {
            assert_eq!(last, &SetupEvent::Installed, "through the proxy");
        } else {
            assert_eq!(
                last,
                &SetupEvent::InstallFailed(InstallFailure::Offline),
                "straight to a host that isn't there"
            );
        }
    }
    assert_eq!(
        *tunnels.lock().unwrap(),
        ["127.0.0.1:9"],
        "one, to where the URL says"
    );
}

/// What comes on `socket` up to the end of a request's head.
async fn head(socket: &mut tokio::net::TcpStream) -> Option<String> {
    let mut asked = Vec::new();
    let mut byte = [0; 1];
    while !asked.ends_with(b"\r\n\r\n") {
        match socket.read(&mut byte).await {
            Ok(1) => asked.push(byte[0]),
            _ => return None,
        }
    }
    Some(String::from_utf8_lossy(&asked).into_owned())
}

/// A proxy on this machine, giving `body` to whatever it's asked through
/// a tunnel, noting where each tunnel was to.
async fn proxy(body: Vec<u8>) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let tunnels = Arc::new(std::sync::Mutex::new(Vec::new()));
    let noted = Arc::clone(&tunnels);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (noted, body) = (Arc::clone(&noted), body.clone());
            tokio::spawn(async move {
                let Some(connect) = head(&mut socket).await else {
                    return;
                };
                let Some(target) = connect.strip_prefix("CONNECT ") else {
                    return;
                };
                let target = target.split(' ').next().unwrap_or_default().to_owned();
                noted.lock().unwrap().push(target);
                let _ = socket
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await;
                // Inside the tunnel, it plays the server.
                if head(&mut socket).await.is_none() {
                    return;
                }
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(answer.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    (url, tunnels)
}

#[tokio::test]
async fn a_download_that_stalls_with_nobody_stopping_it_ends_offline() {
    let dir = Dir::new();
    let body = vec![7; 4 << 20];
    let stalled = serve(body.clone(), true).await.url;
    let mut driver = SetupDriver::with_options(
        dir.root(),
        Options {
            http_to_this_machine: true,
            idle: Some(Duration::from_millis(500)),
            env: Some(std::collections::HashMap::new()),
            ..Options::default()
        },
    );
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1000),
        folder: "fake-1.0.0".into(),
        install: download(&stalled, &body, Unpack::No),
        private: None,
        size: 1 << 30,
    });
    let events = tokio::time::timeout(Duration::from_secs(10), done(&mut driver, Ticket(1000)))
        .await
        .expect("it gives up once nothing comes");
    assert_eq!(
        events.last().expect("a last report"),
        &SetupEvent::InstallFailed(InstallFailure::Offline)
    );
    assert!(installs(&dir).is_empty(), "{:?}", installs(&dir));
    // The installs' lock is let go: another install goes ahead.
    let good = tar_gz();
    let server = serve(good.clone(), false).await;
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1001),
        folder: "fake-1.0.0".into(),
        install: download(&server.url, &good, Unpack::TarGz),
        private: None,
        size: 1 << 30,
    });
    let events = done(&mut driver, Ticket(1001)).await;
    assert_eq!(
        events.last().expect("a last report"),
        &SetupEvent::Installed
    );
}

/// A server on this machine that sends every request elsewhere.
async fn redirect(to: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/fake-adapter", listener.local_addr().unwrap());
    let to = to.to_owned();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let to = to.clone();
            tokio::spawn(async move {
                if head(&mut socket).await.is_some() {
                    let answer = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = socket.write_all(answer.as_bytes()).await;
                }
            });
        }
    });
    url
}

#[tokio::test]
async fn a_failed_download_says_what_went_wrong_but_not_where() {
    let dir = Dir::new();
    let body = tar_gz();
    let server = serve(body.clone(), false).await;
    let elsewhere = redirect("ftp://user:s3cret@files.example/agent?token=s3cret").await;
    let cases = [
        (
            "HTTP_PROXY",
            "socks9://user:s3cret@proxy:1080",
            server.url.clone(),
        ),
        ("HTTP_PROXY", "http://user:s3cret@[::1", server.url.clone()),
        ("NONE", "", elsewhere),
    ];
    for (n, (name, value, url)) in cases.into_iter().enumerate() {
        let env = [(name.to_owned(), value.to_owned())].into_iter().collect();
        let mut driver = SetupDriver::with_options(
            dir.root(),
            Options {
                http_to_this_machine: true,
                env: Some(env),
                ..Options::default()
            },
        );
        let ticket = Ticket(1000 + n as u64);
        driver.apply(SetupDirective::Install {
            ticket,
            folder: "fake-1.0.0".into(),
            install: download(&url, &body, Unpack::TarGz),
            private: None,
            size: 1 << 30,
        });
        let events = done(&mut driver, ticket).await;
        let Some(Ev(SetupEvent::InstallFailed(why))) = events.last() else {
            panic!("{n}: it fails: {events:?}")
        };
        let said = format!("{why:?}");
        assert!(!said.contains("s3cret"), "{n}: {said}");
        if n < 2 {
            assert!(said.contains("HTTP_PROXY"), "{n}: it says which: {said}");
        }
    }
}

#[tokio::test]
async fn a_download_that_stalls_stops_at_once() {
    let dir = Dir::new();
    let body = vec![7; 4 << 20];
    // One that sends half and stalls for good, and one that never answers.
    let stalled = serve(body.clone(), true).await.url;
    let silent = silent().await;
    let mut driver = driver(&dir);
    for (n, url) in [stalled, silent].into_iter().enumerate() {
        let ticket = Ticket(1000 + n as u64);
        driver.apply(SetupDirective::Install {
            ticket,
            folder: "fake-1.0.0".into(),
            install: download(&url, &body, Unpack::No),
            private: None,
            size: 1 << 30,
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        driver.apply(SetupDirective::Stop { ticket });
        let events = tokio::time::timeout(Duration::from_secs(2), async {
            let mut events = Vec::new();
            loop {
                let (about, event) = driver.next().await;
                assert_eq!(about, ticket);
                let last = is_last(&event);
                events.push(Ev(event));
                if last {
                    return events;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{n}: stopped within a second or so"));
        assert_eq!(
            events.last().expect("a last report"),
            &SetupEvent::InstallFailed(InstallFailure::Stopped),
            "{n}"
        );
        assert!(installs(&dir).is_empty(), "{n}: {:?}", installs(&dir));
    }
}

#[tokio::test]
async fn two_drivers_installing_one_folder_install_it_once() {
    let dir = Dir::new();
    let body = tar_gz();
    let server = serve(body.clone(), true).await;
    let mut first = driver(&dir);
    let mut second = driver(&dir);
    for driver in [&mut first, &mut second] {
        driver.apply(SetupDirective::Install {
            ticket: Ticket(1000),
            folder: "fake-1.0.0".into(),
            install: download(&server.url, &body, Unpack::TarGz),
            private: None,
            size: 1 << 30,
        });
    }
    // One has the lock and is downloading; the other waits for it.
    while server.requests.load(Ordering::Relaxed) == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    server.go.notify_waiters();
    let (one, two) = tokio::join!(
        done(&mut first, Ticket(1000)),
        done(&mut second, Ticket(1000))
    );
    assert_eq!(
        one.last().expect("a last report"),
        &(SetupEvent::Installed),
        "{one:?}"
    );
    assert_eq!(
        two.last().expect("a last report"),
        &(SetupEvent::Installed),
        "{two:?}"
    );
    assert_eq!(
        server.requests.load(Ordering::Relaxed),
        1,
        "it was fetched once"
    );
    assert_eq!(installs(&dir), ["fake-1.0.0"]);
}

#[tokio::test]
async fn waiting_for_another_install_can_be_stopped() {
    let dir = Dir::new();
    std::fs::create_dir_all(dir.root()).unwrap();
    let held = std::fs::File::create(dir.root().join(".lock")).unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Install {
        ticket: Ticket(1000),
        folder: "fake-1.0.0".into(),
        install: Install::Download(Vec::new()),
        private: None,
        size: 1 << 30,
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    driver.apply(SetupDirective::Stop {
        ticket: Ticket(1000),
    });
    let events = done(&mut driver, Ticket(1000)).await;
    assert_eq!(events, [SetupEvent::InstallFailed(InstallFailure::Stopped)]);
}

#[tokio::test]
async fn an_install_makes_its_agents_private_folder_and_never_removes_it() {
    let dir = Dir::new();
    let body = tar_gz();
    let server = serve(body.clone(), false).await;
    let mut driver = driver(&dir);
    let install = |ticket, folder: &str, private: &str| SetupDirective::Install {
        ticket: Ticket(ticket),
        folder: folder.into(),
        install: download(&server.url, &body, Unpack::TarGz),
        private: Some(private.into()),
        size: 1 << 30,
    };
    driver.apply(install(1000, "fake-1.0.0", "fake-private"));
    let events = done(&mut driver, Ticket(1000)).await;
    assert_eq!(
        events.last().expect("a last report"),
        &SetupEvent::Installed
    );
    let private = dir.root().join("fake-private");
    let mode = std::fs::symlink_metadata(&private)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700, "the user's alone");
    std::fs::write(private.join("token"), "kept").unwrap();

    // A later version keeps what's in it, as it is.
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o750)).unwrap();
    driver.apply(install(1001, "fake-1.1.0", "fake-private"));
    let events = done(&mut driver, Ticket(1001)).await;
    assert_eq!(
        events.last().expect("a last report"),
        &SetupEvent::Installed
    );
    assert_eq!(
        std::fs::read_to_string(private.join("token")).unwrap(),
        "kept"
    );

    // Neither a survey nor a removal takes it.
    driver.apply(SetupDirective::Remove {
        ticket: Ticket(1002),
        folder: "fake-private".into(),
        unused_since: ANY_TIME,
    });
    assert_eq!(
        done(&mut driver, Ticket(1002)).await,
        [SetupEvent::Removed { removed: false }]
    );
    driver.apply(SetupDirective::Survey {
        ticket: Ticket(1003),
    });
    let events = done(&mut driver, Ticket(1003)).await;
    let [Ev(SetupEvent::Surveyed { installed, .. })] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(installed, &["fake-1.0.0", "fake-1.1.0"]);
    assert!(private.join("token").exists());

    // One that's a link, or that isn't a private folder's name, is
    // refused, and nothing is installed.
    std::os::unix::fs::symlink(dir.path("elsewhere"), dir.root().join("other-private")).unwrap();
    for (n, private) in ["other-private", "../escape-private", "fake-1.0.0"]
        .into_iter()
        .enumerate()
    {
        let ticket = 2000 + n as u64;
        driver.apply(install(ticket, "other-1.0.0", private));
        let events = done(&mut driver, Ticket(ticket)).await;
        assert!(
            matches!(
                events.as_slice(),
                [Ev(SetupEvent::InstallFailed(InstallFailure::Other(_)))]
            ),
            "{private}: {events:?}"
        );
    }
    assert!(
        !dir.path("elsewhere").exists(),
        "nothing was made through the link"
    );
    assert!(!dir.root().join("other-1.0.0").exists());
}

#[tokio::test]
async fn only_install_folders_can_be_installed_or_removed() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    std::fs::create_dir_all(dir.root().join("claude-private")).unwrap();
    for (n, folder) in ["../escape", "claude-private", ".lock", "a/b"]
        .into_iter()
        .enumerate()
    {
        let ticket = Ticket(1000 + n as u64);
        driver.apply(SetupDirective::Install {
            ticket,
            folder: folder.into(),
            install: Install::Download(Vec::new()),
            private: None,
            size: 1 << 30,
        });
        let events = done(&mut driver, ticket).await;
        assert!(
            matches!(
                events.as_slice(),
                [Ev(SetupEvent::InstallFailed(InstallFailure::Other(_)))]
            ),
            "{folder}: {events:?}"
        );
        let ticket = Ticket(2000 + n as u64);
        driver.apply(SetupDirective::Remove {
            ticket,
            folder: folder.into(),
            unused_since: ANY_TIME,
        });
        let events = done(&mut driver, ticket).await;
        assert_eq!(events, [SetupEvent::Removed { removed: false }], "{folder}");
    }
    assert!(dir.root().join("claude-private").exists());
}

#[tokio::test]
async fn an_install_isnt_removed_while_a_process_runs_from_it() {
    let dir = Dir::new();
    let install = dir.root().join("fake-1.0.0");
    std::fs::create_dir_all(&install).unwrap();
    let mut setup = driver(&dir);
    let mut agents = AgentDriver::new(AgentLimits::default());
    let mut command = SpawnCommand::new("/bin/sh");
    command.args = vec!["-c".into(), "exec sleep 30".into()];
    agents.apply(AgentDirective::Start {
        process: Ticket(1000),
        command,
        hold: Some(install.clone()),
    });
    let (_, started) = agents.next().await;
    assert!(matches!(started, AgentEvent::Started { .. }), "{started:?}");
    setup.apply(SetupDirective::Remove {
        ticket: Ticket(1001),
        folder: "fake-1.0.0".into(),
        unused_since: ANY_TIME,
    });
    let events = done(&mut setup, Ticket(1001)).await;
    assert_eq!(events, [SetupEvent::Removed { removed: false }]);
    assert!(install.exists());

    agents.apply(AgentDirective::Signal {
        process: Ticket(1000),
        signal: Signal::Kill,
    });
    loop {
        let (_, event) = tokio::time::timeout(PATIENCE, agents.next()).await.unwrap();
        if matches!(event, AgentEvent::Exited(_)) {
            break;
        }
    }
    // A process another thread starts holds every file open in this one
    // for a moment, until it starts its program, and the lock with it: so
    // a removal may be refused for that moment, never allowed early.
    let mut ticket = 1002;
    loop {
        setup.apply(SetupDirective::Remove {
            ticket: Ticket(ticket),
            folder: "fake-1.0.0".into(),
            unused_since: ANY_TIME,
        });
        let events = done(&mut setup, Ticket(ticket)).await;
        if events == [SetupEvent::Removed { removed: true }] {
            break;
        }
        assert!(ticket < 1100, "it's removed once nothing runs from it");
        ticket += 1;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(installs(&dir).is_empty(), "{:?}", installs(&dir));
    // A folder that isn't there is gone already.
    setup.apply(SetupDirective::Remove {
        ticket: Ticket(2000),
        folder: "fake-1.0.0".into(),
        unused_since: ANY_TIME,
    });
    let events = done(&mut setup, Ticket(2000)).await;
    assert_eq!(events, [SetupEvent::Removed { removed: true }]);
}

#[tokio::test]
async fn a_program_still_open_for_writing_is_waited_for_briefly() {
    // As one just installed may be, for a moment, when another thread
    // starts a process while it's written: running it fails with "text
    // file busy" until that process has started its own program.
    let dir = Dir::new();
    let program = dir.path("agent");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let writing = std::fs::OpenOptions::new()
        .write(true)
        .open(&program)
        .unwrap();
    let mut agents = AgentDriver::new(AgentLimits::default());
    agents.apply(AgentDirective::Start {
        process: Ticket(1000),
        command: SpawnCommand::new(&program),
        hold: None,
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(writing);
    let (_, event) = tokio::time::timeout(PATIENCE, agents.next()).await.unwrap();
    assert!(matches!(event, AgentEvent::Started { .. }), "{event:?}");
}

/// A time later than any folder in these tests was used, so a removal
/// isn't held for it.
const ANY_TIME: caf_core::time::Wall = caf_core::time::Wall(i64::MAX);

#[tokio::test]
async fn a_removal_keeps_a_folder_used_since_the_core_decided() {
    let dir = Dir::new();
    let at = |secs: u64| std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    for (folder, used) in [("fake-0.8.0", 1_600_000_000), ("fake-0.9.0", 1_700_000_000)] {
        let install = dir.root().join(folder);
        std::fs::create_dir_all(&install).unwrap();
        std::fs::File::create(install.join(".used"))
            .unwrap()
            .set_modified(at(used))
            .unwrap();
    }
    let mut driver = driver(&dir);
    for (n, folder, removed) in [(0, "fake-0.8.0", true), (1, "fake-0.9.0", false)] {
        driver.apply(SetupDirective::Remove {
            ticket: Ticket(1000 + n),
            folder: folder.into(),
            unused_since: caf_core::time::Wall(1_650_000_000_000),
        });
        assert_eq!(
            done(&mut driver, Ticket(1000 + n)).await,
            [SetupEvent::Removed { removed }],
            "{folder}"
        );
        assert_eq!(dir.root().join(folder).exists(), !removed, "{folder}");
    }
}

#[tokio::test]
async fn a_survey_says_when_each_install_was_last_used() {
    let dir = Dir::new();
    let at = |secs: u64| std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    let started = dir.root().join("fake-1.0.0");
    std::fs::create_dir_all(&started).unwrap();
    std::fs::File::create(started.join(".used"))
        .unwrap()
        .set_modified(at(1_700_000_000))
        .unwrap();
    // Never started from: when it was installed.
    let never = dir.root().join("fake-0.9.0");
    std::fs::create_dir_all(&never).unwrap();
    std::fs::File::open(&never)
        .unwrap()
        .set_modified(at(1_600_000_000))
        .unwrap();
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Survey {
        ticket: Ticket(1000),
    });
    let events = done(&mut driver, Ticket(1000)).await;
    let [Ev(SetupEvent::Surveyed { used, .. })] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(
        used,
        &[
            (
                "fake-0.9.0".to_owned(),
                caf_core::time::Wall(1_600_000_000_000)
            ),
            (
                "fake-1.0.0".to_owned(),
                caf_core::time::Wall(1_700_000_000_000)
            ),
        ]
    );
}

#[tokio::test]
async fn starting_from_an_install_marks_it_used() {
    let dir = Dir::new();
    let install = dir.root().join("fake-1.0.0");
    std::fs::create_dir_all(&install).unwrap();
    let used = install.join(".used");
    let long_ago = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    std::fs::File::create(&used)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    let mut agents = AgentDriver::new(AgentLimits::default());
    agents.apply(AgentDirective::Start {
        process: Ticket(1000),
        command: SpawnCommand::new("/bin/true"),
        hold: Some(install.clone()),
    });
    let (_, event) = tokio::time::timeout(PATIENCE, agents.next()).await.unwrap();
    assert!(matches!(event, AgentEvent::Started { .. }), "{event:?}");
    let touched = std::fs::metadata(&used).unwrap().modified().unwrap();
    assert!(
        touched > long_ago + Duration::from_secs(60 * 60 * 24 * 365),
        "its use is marked: {touched:?}"
    );
    // One never used before gets the mark.
    std::fs::remove_file(&used).unwrap();
    agents.apply(AgentDirective::Start {
        process: Ticket(1001),
        command: SpawnCommand::new("/bin/true"),
        hold: Some(install.clone()),
    });
    loop {
        let (process, event) = tokio::time::timeout(PATIENCE, agents.next()).await.unwrap();
        if process == Ticket(1001) {
            assert!(matches!(event, AgentEvent::Started { .. }), "{event:?}");
            break;
        }
    }
    assert!(used.exists());
}

#[tokio::test]
async fn a_process_whose_install_is_missing_doesnt_start() {
    let dir = Dir::new();
    let mut agents = AgentDriver::new(AgentLimits::default());
    agents.apply(AgentDirective::Start {
        process: Ticket(1000),
        command: SpawnCommand::new("/bin/true"),
        hold: Some(dir.root().join("fake-1.0.0")),
    });
    let (_, event) = tokio::time::timeout(PATIENCE, agents.next()).await.unwrap();
    assert!(matches!(event, AgentEvent::StartFailed { .. }), "{event:?}");
}

/// The fake agent's `auth` commands, with `auth` as their settings.
fn fake_auth(dir: &Dir, auth: &Auth, args: &[&str]) -> SpawnCommand {
    let settings = dir.path("auth.json");
    std::fs::write(&settings, serde_json::to_string(auth).unwrap()).unwrap();
    let mut command = SpawnCommand::new(caf_fake_agent::binary());
    command.args = args.iter().map(Into::into).collect();
    command.env = vec![EnvChange::Set(AUTH_VAR.into(), settings.into())];
    command
}

/// Whether anything in `events` holds what a sign-in command printed
/// that CAF mustn't keep.
fn leaks(events: &[Ev]) -> bool {
    let text = format!("{events:?}");
    text.contains(AUTH_SECRET) || text.contains(AUTH_EMAIL) || text.contains("Signing in")
}

/// The folders a `BROWSER` was made in that are still there.
fn browsers(dir: &Dir) -> Vec<String> {
    names(&dir.path("temp"))
}

#[tokio::test]
async fn a_sign_in_hands_back_the_link_its_browser_was_given() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    let command = fake_auth(
        &dir,
        &Auth {
            url: Some("https://claude.ai/oauth/authorize?code=true&state=abc".into()),
            browser: true,
            code: Some("ABCD-1234".into()),
            state: Some(dir.path("signed-in")),
            ..Auth::default()
        },
        &["--cli", "auth", "login"],
    );
    driver.apply(SetupDirective::SignIn {
        ticket: Ticket(1000),
        command,
        link: LinkFrom::Browser,
        code_after: Some("Your code: ".into()),
        paste: true,
    });
    let events = done(&mut driver, Ticket(1000)).await;
    let (ends, found) = events.split_last().unwrap();
    assert_eq!(ends, &SetupEvent::Ended { ok: true });
    assert!(
        found.iter().any(|event| *event
            == SetupEvent::Link("https://claude.ai/oauth/authorize?code=true&state=abc".into())),
        "{events:?}"
    );
    assert!(
        found
            .iter()
            .any(|event| *event == SetupEvent::Code("ABCD-1234".into())),
        "{events:?}"
    );
    assert_eq!(found.len(), 2, "each once: {events:?}");
    assert!(!leaks(&events), "{events:?}");
    assert!(dir.path("signed-in").exists());
    let ours = browsers(&dir);
    assert!(ours.is_empty(), "the browser's folder is gone: {ours:?}");
}

#[tokio::test]
async fn a_sign_in_takes_its_link_from_its_output_and_a_pasted_code() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    let command = fake_auth(
        &dir,
        &Auth {
            url: Some("https://example.com/device".into()),
            marker: Some("Open this link: ".into()),
            paste: true,
            expect: Some("pasted-code".into()),
            ..Auth::default()
        },
        &["auth", "login"],
    );
    driver.apply(SetupDirective::SignIn {
        ticket: Ticket(1000),
        command,
        link: LinkFrom::Output("Open this link: ".into()),
        code_after: None,
        paste: true,
    });
    assert_eq!(
        next_of(&mut driver, Ticket(1000)).await,
        SetupEvent::Link("https://example.com/device".into())
    );
    driver.apply(SetupDirective::Paste {
        ticket: Ticket(1000),
        text: "  pasted-code\nand more".into(),
    });
    let events = done(&mut driver, Ticket(1000)).await;
    assert_eq!(events, [SetupEvent::Ended { ok: true }]);
}

#[tokio::test]
async fn a_sign_in_that_takes_no_paste_has_no_input() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    // It asks for a code, but its recipe takes none: it reads the end of
    // its input at once, rather than waiting for a paste that never comes.
    let command = fake_auth(
        &dir,
        &Auth {
            paste: true,
            ..Auth::default()
        },
        &["auth", "login"],
    );
    driver.apply(SetupDirective::SignIn {
        ticket: Ticket(1000),
        command,
        link: LinkFrom::Browser,
        code_after: None,
        paste: false,
    });
    let events = tokio::time::timeout(Duration::from_secs(10), done(&mut driver, Ticket(1000)))
        .await
        .expect("it ends at once, with nothing to read");
    assert_eq!(events, [SetupEvent::Ended { ok: false }]);
}

#[tokio::test]
async fn a_sign_in_that_never_reads_whats_pasted_still_stops() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    let mut command = SpawnCommand::new("/bin/sh");
    command.args = vec!["-c".into(), "exec sleep 30".into()];
    driver.apply(SetupDirective::SignIn {
        ticket: Ticket(1000),
        command,
        link: LinkFrom::Browser,
        code_after: None,
        paste: true,
    });
    // More than its input's pipe holds, so writing waits.
    for _ in 0..40 {
        driver.apply(SetupDirective::Paste {
            ticket: Ticket(1000),
            text: "x".repeat(4000),
        });
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    driver.apply(SetupDirective::Stop {
        ticket: Ticket(1000),
    });
    let events = tokio::time::timeout(Duration::from_secs(3), done(&mut driver, Ticket(1000)))
        .await
        .expect("it stops within a second or so");
    assert_eq!(events, [SetupEvent::Ended { ok: false }]);
}

#[tokio::test]
async fn a_sign_in_that_fails_or_is_stopped_ends_not_ok() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    let wrong = fake_auth(
        &dir,
        &Auth {
            paste: true,
            expect: Some("right".into()),
            ..Auth::default()
        },
        &["auth", "login"],
    );
    driver.apply(SetupDirective::SignIn {
        ticket: Ticket(1000),
        command: wrong,
        link: LinkFrom::Browser,
        code_after: None,
        paste: true,
    });
    driver.apply(SetupDirective::Paste {
        ticket: Ticket(1000),
        text: "wrong".into(),
    });
    let events = done(&mut driver, Ticket(1000)).await;
    assert_eq!(events, [SetupEvent::Ended { ok: false }]);

    let waiting = fake_auth(
        &dir,
        &Auth {
            wait_file: Some(dir.path("never")),
            ..Auth::default()
        },
        &["auth", "login"],
    );
    driver.apply(SetupDirective::SignIn {
        ticket: Ticket(1001),
        command: waiting,
        link: LinkFrom::Browser,
        code_after: None,
        paste: true,
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    driver.apply(SetupDirective::Stop {
        ticket: Ticket(1001),
    });
    let events = tokio::time::timeout(Duration::from_secs(3), done(&mut driver, Ticket(1001)))
        .await
        .expect("stopped within a second or so, not when it gives up waiting");
    assert_eq!(events, [SetupEvent::Ended { ok: false }]);
    // A paste or stop for a sign-in that has ended does nothing.
    driver.apply(SetupDirective::Paste {
        ticket: Ticket(1001),
        text: "late".into(),
    });
    driver.apply(SetupDirective::Stop {
        ticket: Ticket(1001),
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(200), driver.next())
            .await
            .is_err()
    );
    assert!(browsers(&dir).is_empty(), "{:?}", browsers(&dir));
}

#[tokio::test]
async fn a_status_command_gives_only_the_fields_named() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    let auth = Auth {
        state: Some(dir.path("signed-in")),
        method: Some("console".into()),
        ..Auth::default()
    };
    let how = Some((
        "/authMethod".to_owned(),
        vec![
            ("claude.ai".to_owned(), SignedInAs::Account),
            ("console".to_owned(), SignedInAs::ApiKey),
        ],
    ));
    let status = |ticket| SetupDirective::Status {
        ticket,
        command: fake_auth(&dir, &auth, &["--cli", "auth", "status"]),
        signed_in: "/loggedIn".into(),
        how: how.clone(),
    };
    driver.apply(status(Ticket(1000)));
    assert_eq!(
        done(&mut driver, Ticket(1000)).await,
        [SetupEvent::Status(None)],
        "signed out, though it exits with 1"
    );
    driver.apply(SetupDirective::SignIn {
        ticket: Ticket(1001),
        command: fake_auth(&dir, &auth, &["auth", "login"]),
        link: LinkFrom::Browser,
        code_after: None,
        paste: true,
    });
    assert_eq!(
        done(&mut driver, Ticket(1001)).await,
        [SetupEvent::Ended { ok: true }]
    );
    driver.apply(status(Ticket(1002)));
    let events = done(&mut driver, Ticket(1002)).await;
    assert_eq!(events, [SetupEvent::Status(Some(SignedInAs::ApiKey))]);
    assert!(!leaks(&events));

    driver.apply(SetupDirective::Run {
        ticket: Ticket(1003),
        command: fake_auth(&dir, &auth, &["auth", "logout"]),
    });
    assert_eq!(
        done(&mut driver, Ticket(1003)).await,
        [SetupEvent::Ended { ok: true }]
    );
    driver.apply(status(Ticket(1004)));
    assert_eq!(
        done(&mut driver, Ticket(1004)).await,
        [SetupEvent::Status(None)]
    );

    // One stopped ends, not ok.
    let mut command = SpawnCommand::new("/bin/sh");
    command.args = vec!["-c".into(), "exec sleep 30".into()];
    driver.apply(SetupDirective::Status {
        ticket: Ticket(1010),
        command,
        signed_in: "/loggedIn".into(),
        how: None,
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    driver.apply(SetupDirective::Stop {
        ticket: Ticket(1010),
    });
    assert_eq!(
        done(&mut driver, Ticket(1010)).await,
        [SetupEvent::Ended { ok: false }]
    );

    // An answer without the field: a failure, in CAF's words.
    driver.apply(SetupDirective::Status {
        ticket: Ticket(1005),
        command: fake_auth(&dir, &auth, &["auth", "status"]),
        signed_in: "/signedIn".into(),
        how: None,
    });
    let events = done(&mut driver, Ticket(1005)).await;
    assert!(
        matches!(events.as_slice(), [Ev(SetupEvent::Failed(_))]),
        "{events:?}"
    );
    assert!(!leaks(&events));
}

#[tokio::test]
async fn a_status_command_that_says_too_much_is_cut_off() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    let mut command = SpawnCommand::new("/bin/sh");
    command.args = vec!["-c".into(), "exec yes".into()];
    driver.apply(SetupDirective::Status {
        ticket: Ticket(1000),
        command,
        signed_in: "/loggedIn".into(),
        how: None,
    });
    let events = tokio::time::timeout(Duration::from_secs(10), done(&mut driver, Ticket(1000)))
        .await
        .expect("it ends once its answer is too long, not when it times out");
    assert!(
        matches!(events.as_slice(), [Ev(SetupEvent::Failed(_))]),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_sign_in_ends_though_what_it_started_holds_its_output() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    let mut command = SpawnCommand::new("/bin/sh");
    command.args = vec![
        "-c".into(),
        "echo 'Open: https://example.com/a'; sleep 30 & exit 0".into(),
    ];
    driver.apply(SetupDirective::SignIn {
        ticket: Ticket(1000),
        command,
        link: LinkFrom::Output("Open: ".into()),
        code_after: None,
        paste: true,
    });
    let events = tokio::time::timeout(Duration::from_secs(10), done(&mut driver, Ticket(1000)))
        .await
        .expect("it ends with its command");
    assert_eq!(
        events,
        [
            SetupEvent::Link("https://example.com/a".into()),
            SetupEvent::Ended { ok: true }
        ]
    );
}

#[tokio::test]
async fn a_status_command_is_read_though_what_it_started_holds_its_output() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    let mut command = SpawnCommand::new("/bin/sh");
    command.args = vec![
        "-c".into(),
        r#"echo '{"loggedIn": true}'; sleep 30 & exit 0"#.into(),
    ];
    driver.apply(SetupDirective::Status {
        ticket: Ticket(1000),
        command,
        signed_in: "/loggedIn".into(),
        how: None,
    });
    let events = tokio::time::timeout(Duration::from_secs(5), done(&mut driver, Ticket(1000)))
        .await
        .expect("its answer is read once it has ended");
    assert_eq!(events, [SetupEvent::Status(Some(SignedInAs::Unknown))]);
}

#[tokio::test]
async fn a_command_that_cant_start_fails() {
    let dir = Dir::new();
    let mut driver = driver(&dir);
    driver.apply(SetupDirective::Run {
        ticket: Ticket(1000),
        command: SpawnCommand::new(dir.path("missing")),
    });
    let events = done(&mut driver, Ticket(1000)).await;
    assert!(
        matches!(events.as_slice(), [Ev(SetupEvent::Failed(_))]),
        "{events:?}"
    );
    driver.apply(SetupDirective::Run {
        ticket: Ticket(1001),
        command: SpawnCommand::new("/bin/false"),
    });
    assert_eq!(
        done(&mut driver, Ticket(1001)).await,
        [SetupEvent::Ended { ok: false }]
    );
}

#[test]
fn the_default_root_is_the_users_data_folder() {
    // Read from this process's own environment, which tests don't change.
    let root = SetupDriver::default_root();
    let expected = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|data| data.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|data| data.join("caf/agents"));
    assert_eq!(root, expected);
}
