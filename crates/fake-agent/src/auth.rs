//! `caf-fake-agent auth …`: a fake sign-in, sign-out and status, as an
//! agent's own command line has them. See the crate's docs.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{AUTH_VAR, EXIT_BAD_SCRIPT};

/// What `auth login`, `auth status` and `auth logout` do: the JSON file
/// [`AUTH_VAR`] names. Every field may be left out.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Auth {
    /// The sign-in link `auth login` gives.
    pub url: Option<String>,
    /// Whether `auth login` opens the link with `$BROWSER`, as a program
    /// given the link as its only argument.
    pub browser: bool,
    /// If set, `auth login` prints a line with this, then the link, on
    /// its standard output.
    pub marker: Option<String>,
    /// A code `auth login` shows, on a line of its error output after
    /// [`Auth::code_after`].
    pub code: Option<String>,
    /// What comes before the code on its line. `Your code: ` by default.
    pub code_after: Option<String>,
    /// Whether `auth login` asks for a code to be pasted, and waits for a
    /// line on its standard input. Any line will do, unless
    /// [`Auth::expect`] says which.
    pub paste: bool,
    /// The only line pasted that signs in: any other, or none, fails.
    pub expect: Option<String>,
    /// A file `auth login` waits for before it ends, as a sign-in waits
    /// for the user to approve it in their browser: the test makes it.
    pub wait_file: Option<PathBuf>,
    /// How long `auth login` waits for [`Auth::wait_file`], in
    /// milliseconds. A minute by default. Past it, it fails.
    pub timeout: Option<u64>,
    /// The code `auth login` exits with, if nothing else failed: 0 by
    /// default.
    pub exit: Option<i32>,
    /// Where the fake keeps whether it's signed in: `auth login` writes it
    /// as it succeeds, `auth logout` removes it, and `auth status` reads
    /// it. Without one, `auth status` says it's signed out.
    pub state: Option<PathBuf>,
    /// How `auth login` signs in, as `auth status` reports it in
    /// `authMethod`: `claude.ai` by default, as Claude's does.
    pub method: Option<String>,
}

/// The account `auth login` and `auth status` name, so a test can check
/// it never reaches anything CAF keeps.
pub const AUTH_EMAIL: &str = "fake-account@example.com";

/// A secret `auth login` prints, for the same reason.
pub const AUTH_SECRET: &str = "sk-fake-sign-in-secret";

/// Runs `auth` with `args`, the words after it, such as `login`. Returns
/// the exit code.
#[must_use]
pub fn auth(args: &[String]) -> i32 {
    let auth = match load() {
        Ok(auth) => auth,
        Err(why) => {
            eprintln!("caf-fake-agent auth: {why}");
            return EXIT_BAD_SCRIPT;
        }
    };
    match args.first().map(String::as_str) {
        Some("login") => login(&auth),
        Some("status") => status(&auth),
        Some("logout") => {
            if let Some(state) = &auth.state {
                let _ = std::fs::remove_file(state);
            }
            println!("Signed out of {AUTH_EMAIL}.");
            0
        }
        _ => {
            eprintln!("usage: caf-fake-agent [--cli] auth login|status|logout");
            EXIT_BAD_SCRIPT
        }
    }
}

/// The settings in [`AUTH_VAR`]'s file.
fn load() -> Result<Auth, String> {
    let Some(path) = std::env::var_os(AUTH_VAR) else {
        return Err(format!("{AUTH_VAR} isn't set"));
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("{}: {error}", Path::new(&path).display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{}: {error}", Path::new(&path).display()))
}

fn login(auth: &Auth) -> i32 {
    let mut out = std::io::stdout();
    // Noise a sign-in command prints, which must never reach CAF.
    let _ = writeln!(out, "Signing in… (session token {AUTH_SECRET})");
    eprintln!("Using account {AUTH_EMAIL}");
    if let Some(code) = &auth.code {
        let before = auth.code_after.as_deref().unwrap_or("Your code: ");
        eprintln!("{before}{code}");
    }
    if let Some(url) = &auth.url {
        if let Some(marker) = &auth.marker {
            let _ = writeln!(out, "{marker}{url}");
        }
        let _ = out.flush();
        if auth.browser {
            let Some(browser) = std::env::var_os("BROWSER") else {
                eprintln!("caf-fake-agent auth: BROWSER isn't set");
                return 1;
            };
            match std::process::Command::new(browser).arg(url).status() {
                Ok(status) if status.success() => {}
                _ => {
                    eprintln!("caf-fake-agent auth: the browser failed");
                    return 1;
                }
            }
        }
    }
    if auth.paste {
        let _ = write!(out, "Paste code here if prompted > ");
        let _ = out.flush();
        let mut line = String::new();
        let read = std::io::stdin().lock().read_line(&mut line);
        let line = line.trim_end_matches(['\n', '\r']);
        let pasted = matches!(read, Ok(n) if n > 0);
        if !pasted || auth.expect.as_deref().is_some_and(|expect| expect != line) {
            eprintln!("caf-fake-agent auth: that code isn't right");
            return 1;
        }
    }
    if let Some(file) = &auth.wait_file {
        let timeout = Duration::from_millis(auth.timeout.unwrap_or(60_000));
        let until = Instant::now().checked_add(timeout);
        while !file.exists() {
            if until.is_some_and(|until| Instant::now() >= until) {
                eprintln!("caf-fake-agent auth: nobody approved the sign-in");
                return 1;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let exit = auth.exit.unwrap_or(0);
    if exit == 0
        && let Some(state) = &auth.state
    {
        let method = auth.method.as_deref().unwrap_or("claude.ai");
        if std::fs::write(state, method).is_err() {
            eprintln!("caf-fake-agent auth: can't keep the sign-in");
            return 1;
        }
    }
    let _ = writeln!(out, "Signed in as {AUTH_EMAIL}.");
    exit
}

fn status(auth: &Auth) -> i32 {
    let method = (auth.state.as_ref()).and_then(|state| std::fs::read_to_string(state).ok());
    let answer = match &method {
        Some(method) => serde_json::json!({
            "loggedIn": true,
            "authMethod": method,
            "email": AUTH_EMAIL,
            "orgName": "Fake Org",
        }),
        None => serde_json::json!({ "loggedIn": false, "authMethod": "none" }),
    };
    println!("{answer}");
    i32::from(method.is_none())
}
