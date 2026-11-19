//! Finding the program, for tests in other crates.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::BIN_VAR;

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
/// Once per process, it:
/// 1. gives the program [`BIN_VAR`] names, if it's set, building nothing;
/// 2. otherwise looks in the folder Cargo put the test program in,
///    `target/<profile>/`, or `target/<triple>/<profile>/` when built with
///    `--target`, wherever the target folder is;
/// 3. takes the program there as it is if it's newer than each of its
///    source files (as its `.d` file lists them), its `Cargo.toml` and the
///    workspace's `Cargo.lock`;
/// 4. if not, runs `cargo build -p caf-fake-agent` for the same profile,
///    target and target folder, with the Cargo that runs the tests
///    (`CARGO`).
///
/// So it runs Cargo only when the program is missing or stale, and never
/// replaces a program other tests may be starting otherwise. Cargo's
/// `--config` flags aren't passed on: a build that needs them should set
/// [`BIN_VAR`].
///
/// # Panics
///
/// If [`BIN_VAR`] isn't an absolute path to a file; if the folder isn't
/// one Cargo made; if the program is missing or stale and there's no
/// `CARGO` to build it with; or if the build fails.
#[must_use]
pub fn binary() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(find).clone()
}

fn find() -> PathBuf {
    if let Some(path) = std::env::var_os(BIN_VAR) {
        let path = PathBuf::from(path);
        assert!(
            path.is_absolute() && path.is_file(),
            "{BIN_VAR} must be an absolute path to the program, not {}",
            path.display()
        );
        return path;
    }
    let place = Place::of_this_test();
    let path = place.profile_dir.join("caf-fake-agent");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let others = [
        manifest.join("Cargo.toml"),
        manifest.join("../../Cargo.lock"),
    ];
    if fresh(&path, &others) {
        return path;
    }
    let Some(cargo) = std::env::var_os("CARGO") else {
        panic!(
            "{} is missing or older than its sources, and there's no CARGO to build it with: \
             run `cargo build -p caf-fake-agent`, or set {BIN_VAR}",
            path.display()
        );
    };
    let mut build = Command::new(cargo);
    build
        .args(["build", "--quiet", "-p", "caf-fake-agent", "--target-dir"])
        .arg(&place.target_dir)
        .current_dir(manifest);
    if let Some(triple) = &place.triple {
        build.args(["--target", triple]);
    }
    match place.profile.as_str() {
        "debug" => {}
        "release" => {
            build.arg("--release");
        }
        profile => {
            build.args(["--profile", profile]);
        }
    }
    let status = build.status().expect("cargo runs");
    assert!(status.success(), "`cargo build -p caf-fake-agent` failed");
    assert!(
        path.is_file(),
        "{} isn't there after building it",
        path.display()
    );
    path
}

/// Where Cargo put the test program.
struct Place {
    /// The target folder.
    target_dir: PathBuf,
    /// The target it was built for with `--target`, if any.
    triple: Option<String>,
    /// The profile's folder name, such as `debug`.
    profile: String,
    /// The profile's folder, where programs go.
    profile_dir: PathBuf,
}

impl Place {
    fn of_this_test() -> Self {
        let test = std::env::current_exe().expect("the test program has a path");
        let mut profile_dir = test.parent().expect("it's in a folder").to_path_buf();
        // Tests are in `deps`, beside the programs.
        if profile_dir.ends_with("deps") {
            profile_dir.pop();
        }
        let profile = name(&profile_dir);
        // Cargo marks the target folder with this file.
        let marked = |dir: &Path| dir.join("CACHEDIR.TAG").is_file();
        let parent = profile_dir.parent().map(Path::to_path_buf);
        let grandparent = parent.as_deref().and_then(Path::parent);
        let (target_dir, triple) = match (parent.as_deref(), grandparent) {
            (Some(parent), _) if marked(parent) => (parent.to_path_buf(), None),
            (Some(parent), Some(grandparent)) if marked(grandparent) => {
                (grandparent.to_path_buf(), Some(name(parent)))
            }
            _ => panic!(
                "{} isn't in a folder Cargo made: set {BIN_VAR}",
                test.display()
            ),
        };
        Self {
            target_dir,
            triple,
            profile,
            profile_dir,
        }
    }
}

fn name(dir: &Path) -> String {
    dir.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned()
}

/// Whether the program is there and newer than each source file its `.d`
/// file lists, and than `others`. A `.d` file that's missing or names
/// nothing means stale.
fn fresh(program: &Path, others: &[PathBuf]) -> bool {
    let modified = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    };
    let Some(built) = modified(program) else {
        return false;
    };
    let Ok(dep_info) = std::fs::read_to_string(program.with_extension("d")) else {
        return false;
    };
    let sources = sources(&dep_info);
    let older = |path: &Path| modified(path).is_some_and(|time: SystemTime| time <= built);
    !sources.is_empty()
        && sources.iter().all(|source| older(source))
        && others.iter().all(|other| !other.exists() || older(other))
}

/// The files a Make-style `.d` file says its target depends on. Spaces in
/// names are escaped as `\ `.
fn sources(dep_info: &str) -> Vec<PathBuf> {
    let Some(first) = dep_info.lines().next() else {
        return Vec::new();
    };
    let Some((_, deps)) = first.split_once(": ") else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    let mut path = String::new();
    let mut chars = deps.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => path.extend(chars.next()),
            ' ' => {
                if !path.is_empty() {
                    paths.push(PathBuf::from(std::mem::take(&mut path)));
                }
            }
            c => path.push(c),
        }
    }
    if !path.is_empty() {
        paths.push(PathBuf::from(path));
    }
    paths
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use super::{fresh, sources};

    #[test]
    fn dep_info_names_its_sources() {
        assert_eq!(
            sources("/t/debug/caf-fake-agent: /src/main.rs /my\\ dir/lib.rs\n\n/src/main.rs:\n"),
            [
                PathBuf::from("/src/main.rs"),
                PathBuf::from("/my dir/lib.rs")
            ]
        );
        assert!(sources("").is_empty());
    }

    #[test]
    fn a_program_older_than_a_source_is_stale() {
        let dir = std::env::temp_dir().join(format!("caf-fake-agent-fresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("caf-fake-agent");
        let source = dir.join("main.rs");
        let lock = dir.join("Cargo.lock");
        let touch = |path: &Path, secs: u64| {
            let file = File::create(path).unwrap();
            file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
        };
        let others = [lock.clone()];
        assert!(!fresh(&program, &others), "missing");
        touch(&program, 2000);
        assert!(!fresh(&program, &others), "no .d file");
        std::fs::write(
            program.with_extension("d"),
            format!("{}: {}\n", program.display(), source.display()),
        )
        .unwrap();
        touch(&source, 1000);
        assert!(fresh(&program, &others));
        touch(&lock, 3000);
        assert!(!fresh(&program, &others), "the lock file changed");
        touch(&lock, 1000);
        touch(&source, 3000);
        assert!(!fresh(&program, &others), "a source changed");
        std::fs::remove_file(&source).unwrap();
        assert!(!fresh(&program, &others), "a source went");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
