//! Finding the program, for tests in other crates.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// The path of the `caf-fake-agent` program, built first if it's missing
/// or stale.
///
/// Cargo tells only this crate's own tests where the program is, in
/// `CARGO_BIN_EXE_caf-fake-agent`. Tests in other crates, which have this
/// crate as a dev-dependency, call this instead:
///
/// ```no_run
/// let fake = caf_fake_agent::binary();
/// let status = std::process::Command::new(&fake)
///     .arg("/tmp/test-1234/script.jsonl")
///     .status()
///     .unwrap();
/// ```
///
/// It looks beside the test program, in `target/<profile>/`. While tests
/// run under Cargo, which says where it is in `CARGO`, it first runs
/// `cargo build -p caf-fake-agent` for the same profile, once per process:
/// Cargo rebuilds the program only if it's missing or stale, and otherwise
/// takes a moment. Run without Cargo, it uses the program as it finds it.
/// A test built for another target (`--target`) must build the program
/// itself.
///
/// # Panics
///
/// If the build fails, or the program still isn't there.
#[must_use]
pub fn binary() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(find).clone()
}

fn find() -> PathBuf {
    let test = std::env::current_exe().expect("the test program has a path");
    let mut dir = test.parent().expect("it's in a folder").to_path_buf();
    // Integration tests and unit tests are in `deps`, beside the programs.
    if dir.ends_with("deps") {
        dir.pop();
    }
    if let Some(cargo) = std::env::var_os("CARGO") {
        let mut build = Command::new(cargo);
        build
            .args(["build", "--quiet", "-p", "caf-fake-agent"])
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        match dir.file_name().and_then(|name| name.to_str()) {
            Some("debug") | None => {}
            Some("release") => {
                build.arg("--release");
            }
            Some(profile) => {
                build.args(["--profile", profile]);
            }
        }
        let status = build.status().expect("cargo runs");
        assert!(status.success(), "`cargo build -p caf-fake-agent` failed");
    }
    let path = dir.join("caf-fake-agent");
    assert!(
        path.is_file(),
        "{} isn't there: build it with `cargo build -p caf-fake-agent`",
        path.display()
    );
    path
}
