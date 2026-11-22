//! No test keeps the user's usage ledger: `Config::new` keeps it in the
//! folder every CAF app shares, so each test's config must say otherwise.

use std::path::{Path, PathBuf};

/// Every Rust file under `crates/`.
fn sources() -> Vec<PathBuf> {
    let crates = (Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize())
    .unwrap();
    let mut found = Vec::new();
    let mut folders = vec![crates];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            if path.is_dir() {
                if name != "target" && name != "node_modules" {
                    folders.push(path);
                }
            } else if name.ends_with(".rs") {
                found.push(path);
            }
        }
    }
    assert!(found.len() > 50, "the workspace's files: {found:?}");
    found
}

/// Whether `line` makes a CAF `Config`: `Config::new(` with its
/// arguments, and not another type's, such as `UsageConfig::new(`.
fn makes_config(line: &str) -> bool {
    let code = line.trim_start();
    if code.starts_with("//") {
        return false;
    }
    code.match_indices("Config::new(").any(|(at, _)| {
        let before = code[..at].chars().next_back();
        let after = code[at + "Config::new(".len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_') && after != Some(')')
    })
}

/// Whether `line` sets a config's usage ledger, not a viewer's right to
/// read usage, say.
fn says_usage(line: &str) -> bool {
    line.contains("config.usage =")
        || line.contains(".usage = None")
        || line.contains(".usage = Some")
}

#[test]
fn every_config_made_says_where_its_usage_goes() {
    let mut unsaid = Vec::new();
    // This file's own examples aside.
    for file in sources()
        .into_iter()
        .filter(|file| !file.ends_with("tests/ledger.rs"))
    {
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            // Within the statement, or the few after it.
            let said = lines[at..lines.len().min(at + 8)]
                .iter()
                .any(|line| says_usage(line));
            if makes_config(line) && !said {
                unsaid.push(format!("{}:{}", file.display(), at + 1));
            }
        }
    }
    assert!(
        unsaid.is_empty(),
        "each of these makes a Config and leaves its usage ledger the user's: \
         set `config.usage` beside it, or use the tests' `config_for`: {unsaid:#?}"
    );
}

#[test]
fn no_test_asks_for_the_users_ledger() {
    let mut asking = Vec::new();
    for file in sources() {
        let path = file.to_string_lossy().into_owned();
        // caf-usage's own tests check where the folder is.
        if path.contains("/crates/usage/") || file.ends_with("tests/ledger.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let in_tests = path.contains("/tests/") || file.ends_with("tests.rs");
        // A file's own tests come at its end.
        let from = if in_tests {
            0
        } else {
            match text.find("mod tests {") {
                Some(at) => at,
                None => continue,
            }
        };
        for line in text[from..].lines() {
            if line.contains("for_user()") || line.contains("default_folder()") {
                asking.push(format!("{path}: {}", line.trim()));
            }
        }
    }
    assert!(
        asking.is_empty(),
        "tests asking for the user's ledger: {asking:#?}"
    );
}

#[test]
fn a_config_made_without_saying_is_found() {
    assert!(makes_config(
        "    let config = Config::new(host(), agents);"
    ));
    assert!(makes_config("Caf::start(Config::new(host, vec![]))"));
    assert!(!makes_config("let usage = UsageConfig::new(folder);"));
    assert!(!makes_config("let config = Config::new()"), "ts-rs's");
    assert!(!makes_config("//! Caf::start(Config::new(host, agents))"));
    assert!(says_usage("    config.usage = None;"));
    assert!(says_usage("    config.usage = Some(scratch_ledger(dir));"));
    assert!(!says_usage("    rules.usage = true;"), "a viewer's right");
}
