//! The fake's sign-in commands: `auth login`, `auth status` and
//! `auth logout`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use caf_fake_agent::{AUTH_EMAIL, AUTH_SECRET, AUTH_VAR, Auth, SCRIPT_VAR};

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(1000);
        let dir = std::env::temp_dir().join(format!(
            "caf-fake-auth-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Writes `auth` as the settings, and returns their path.
    fn settings(&self, auth: &Auth) -> PathBuf {
        let path = self.path("auth.json");
        std::fs::write(&path, serde_json::to_string(auth).unwrap()).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs the fake with `args`, these settings, and `input` on stdin.
fn fake(settings: &Path, args: &[&str], env: &[(&str, &Path)], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_caf-fake-agent"))
        .args(args)
        .env(AUTH_VAR, settings)
        .envs(env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.as_bytes()).unwrap();
    drop(stdin);
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_sign_in_prints_its_link_and_code_and_is_kept() {
    let dir = Dir::new();
    let settings = dir.settings(&Auth {
        url: Some("https://example.com/sign-in".into()),
        marker: Some("Open: ".into()),
        code: Some("ABCD-1234".into()),
        state: Some(dir.path("state")),
        ..Auth::default()
    });
    let status = fake(&settings, &["--cli", "auth", "status"], &[], "");
    assert_eq!(status.status.code(), Some(1), "signed out at first");
    let answer: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(answer["loggedIn"], false);

    let login = fake(&settings, &["auth", "login"], &[], "");
    assert_eq!(login.status.code(), Some(0), "{}", text(&login.stderr));
    let (out, err) = (text(&login.stdout), text(&login.stderr));
    assert!(out.contains("Open: https://example.com/sign-in\n"), "{out}");
    assert!(err.contains("Your code: ABCD-1234\n"), "{err}");
    assert!(
        out.contains(AUTH_SECRET) && err.contains(AUTH_EMAIL),
        "it says what CAF mustn't keep"
    );

    let status = fake(&settings, &["--cli", "auth", "status"], &[], "");
    assert_eq!(status.status.code(), Some(0));
    let answer: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(answer["loggedIn"], true);
    assert_eq!(answer["authMethod"], "claude.ai");
    assert_eq!(answer["email"], AUTH_EMAIL);

    let logout = fake(&settings, &["auth", "logout"], &[], "");
    assert_eq!(logout.status.code(), Some(0));
    let status = fake(&settings, &["auth", "status"], &[], "");
    assert_eq!(status.status.code(), Some(1), "signed out again");
}

#[test]
fn a_sign_in_opens_its_link_with_the_browser_it_was_given() {
    let dir = Dir::new();
    let browser = dir.path("browser");
    std::fs::write(
        &browser,
        format!(
            "#!/bin/sh\nprintf '%s' \"$1\" > {}\n",
            dir.path("opened").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &browser,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let settings = dir.settings(&Auth {
        url: Some("https://example.com/sign-in?x=1".into()),
        browser: true,
        ..Auth::default()
    });
    let login = fake(&settings, &["auth", "login"], &[("BROWSER", &browser)], "");
    assert_eq!(login.status.code(), Some(0), "{}", text(&login.stderr));
    assert_eq!(
        std::fs::read_to_string(dir.path("opened")).unwrap(),
        "https://example.com/sign-in?x=1"
    );
    // With no browser, it fails.
    let login = fake(&settings, &["auth", "login"], &[], "");
    assert_eq!(login.status.code(), Some(1));
}

#[test]
fn a_sign_in_waits_for_the_code_pasted() {
    let dir = Dir::new();
    let settings = dir.settings(&Auth {
        paste: true,
        expect: Some("the-code".into()),
        state: Some(dir.path("state")),
        ..Auth::default()
    });
    let wrong = fake(&settings, &["auth", "login"], &[], "not-it\n");
    assert_eq!(wrong.status.code(), Some(1));
    assert!(!dir.path("state").exists(), "nobody is signed in");
    let none = fake(&settings, &["auth", "login"], &[], "");
    assert_eq!(none.status.code(), Some(1), "nothing pasted");
    let right = fake(&settings, &["auth", "login"], &[], "the-code\n");
    assert_eq!(right.status.code(), Some(0), "{}", text(&right.stderr));
    assert!(text(&right.stdout).contains("Paste code here"));
    assert!(dir.path("state").exists());
}

#[test]
fn a_sign_in_exits_as_it_is_told() {
    let dir = Dir::new();
    let settings = dir.settings(&Auth {
        exit: Some(1),
        state: Some(dir.path("state")),
        ..Auth::default()
    });
    let login = fake(&settings, &["auth", "login"], &[], "");
    assert_eq!(login.status.code(), Some(1));
    assert!(
        !dir.path("state").exists(),
        "a failed sign-in signs nobody in"
    );
}

#[test]
fn a_sign_in_waits_for_its_approval() {
    let dir = Dir::new();
    let approved = dir.path("approved");
    let settings = dir.settings(&Auth {
        wait_file: Some(approved.clone()),
        timeout: Some(200),
        ..Auth::default()
    });
    let late = fake(&settings, &["auth", "login"], &[], "");
    assert_eq!(late.status.code(), Some(1), "nobody approved it in time");
    std::fs::write(&approved, "").unwrap();
    let approved = fake(&settings, &["auth", "login"], &[], "");
    assert_eq!(approved.status.code(), Some(0));
}

#[test]
fn auth_comes_before_any_script() {
    let dir = Dir::new();
    let settings = dir.settings(&Auth::default());
    let script = dir.path("script.jsonl");
    std::fs::write(&script, "{\"exit\": 7}\n").unwrap();
    let status = fake(
        &settings,
        &["--cli", "auth", "status"],
        &[(SCRIPT_VAR, &script)],
        "",
    );
    assert_eq!(status.status.code(), Some(1), "it ran auth, not the script");
    assert!(text(&status.stdout).contains("loggedIn"));
}

#[test]
fn auth_after_the_script_it_was_started_with_is_auth() {
    // A host that starts the fake as `caf-fake-agent SCRIPT` keeps those
    // arguments when CAF runs its sign-in, and adds the recipe's after them.
    let dir = Dir::new();
    let settings = dir.settings(&Auth::default());
    let script = dir.path("script.jsonl");
    std::fs::write(&script, "{\"exit\": 7}\n").unwrap();
    let script = script.to_str().unwrap();
    for args in [
        [script, "auth", "status"].as_slice(),
        [script, "--cli", "auth", "status"].as_slice(),
    ] {
        let status = fake(&settings, args, &[], "");
        assert_eq!(
            status.status.code(),
            Some(1),
            "{args:?} ran auth, not the script"
        );
        assert!(text(&status.stdout).contains("loggedIn"), "{args:?}");
    }
    let login = fake(&settings, &[script, "auth", "login"], &[], "");
    assert_eq!(login.status.code(), Some(0), "{}", text(&login.stderr));
    // And the script alone is still a script.
    assert_eq!(fake(&settings, &[script], &[], "").status.code(), Some(7));
}
