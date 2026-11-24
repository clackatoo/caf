use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// A folder of the test's own in the temporary folder, removed when
/// dropped.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static NO: AtomicU64 = AtomicU64::new(0);
        let no = NO.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("cr{}-{no}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(&path).unwrap())
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A log whose file is `path`, keeping at most `most` of it.
fn log_into(path: &Path, most: u64) -> Log {
    let path = path.to_owned();
    Log::new(
        Box::new(move || {
            let file = File::create(&path).map_err(|e| e.to_string())?;
            Ok((path.clone(), file))
        }),
        most,
    )
}

/// `n` lines, numbered from `from`.
fn lines(from: usize, n: usize) -> Vec<String> {
    (from..from + n).map(|n| format!("line {n}")).collect()
}

#[test]
fn a_file_of_output_stops_at_its_most_and_says_so() {
    let dir = Dir::new();
    let path = dir.0.join("out.txt");
    let mut log = log_into(&path, 100);
    log.keep_in_file();
    log.redraw(0, &lines(0, 100));
    let (text, chars, saved) = log.finish();
    assert_eq!(saved.as_deref(), Some(path.as_path()));
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(
        file.ends_with("line 12\n[output beyond 64 MiB not kept]\n"),
        "{file}"
    );
    assert!(file.len() <= 100 + NOT_KEPT.len());
    // What the model's told counts it all.
    assert!(text.starts_with("line 0\nline 1\n"));
    assert_eq!(
        chars,
        lines(0, 100)
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
    );
}

#[test]
fn a_file_of_output_under_its_most_has_it_all() {
    let dir = Dir::new();
    let path = dir.0.join("out.txt");
    let mut log = log_into(&path, MOST_SAVED);
    log.keep_in_file();
    log.redraw(0, &lines(0, 30));
    // Those still drawn are written as it goes.
    log.flush();
    let expected: String = lines(0, 30)
        .iter()
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    log.finish();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
}

#[test]
fn only_folders_no_runner_holds_are_swept() {
    use rustix::fs::{FlockOperation, flock};
    let dir = Dir::new();
    for name in ["gone", "live"] {
        std::fs::create_dir_all(dir.0.join(name).join("chat")).unwrap();
    }
    // A runner that's still there, in whatever namespace, holds its own.
    let live = File::open(dir.0.join("live")).unwrap();
    flock(&live, FlockOperation::LockExclusive).unwrap();
    let mine = dir.0.join("mine");
    let held = claim(&dir.0, &mine).unwrap();
    let mut left: Vec<String> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, [SWEEPING, "live", "mine"]);
    // Its own is held, so the next runner leaves it be.
    let next = claim(&dir.0, &dir.0.join("next")).unwrap();
    assert!(mine.exists());
    drop((held, next, live));
}

#[test]
fn a_file_is_known_by_its_real_path() {
    let dir = Dir::new();
    std::fs::create_dir(dir.0.join("real")).unwrap();
    std::os::unix::fs::symlink("real", dir.0.join("link")).unwrap();
    std::fs::write(dir.0.join("real/a"), "").unwrap();
    let real = dir.0.join("real/a");
    assert_eq!(known_as(&dir.0.join("link/a")), real);
    assert_eq!(known_as(&dir.0.join("real/./a")), real);
    // One not made yet, by its folder's real path.
    assert_eq!(known_as(&dir.0.join("link/new")), dir.0.join("real/new"));
    // Nowhere at all, as it's given.
    assert_eq!(known_as(Path::new("/no/such/a")), Path::new("/no/such/a"));
}
