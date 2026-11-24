use std::os::unix::fs::MetadataExt as _;

use super::*;

/// A folder of the test's own in the temporary folder, removed when
/// dropped.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static NO: AtomicU64 = AtomicU64::new(0);
        let no = NO.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("cf{}-{no}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// The names in it, sorted.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn read_at(path: &Path) -> Result<FileRead, FileError> {
    DiskFiles.read(path).await
}

async fn write_at(
    path: &Path,
    bytes: &[u8],
    expected: Option<Version>,
) -> Result<Version, FileError> {
    DiskFiles
        .write(path, FileText::new(bytes.to_vec()), expected, CallId(1))
        .await
}

#[tokio::test]
async fn a_file_reads_as_its_bytes_and_a_version_that_moves_when_it_changes() {
    let dir = Dir::new();
    let path = dir.path("a.txt");
    std::fs::write(&path, b"one\n").unwrap();
    let first = read_at(&path).await.unwrap();
    assert_eq!(first.bytes, b"one\n");
    assert_eq!(read_at(&path).await.unwrap().version, first.version);
    let written = write_at(&path, b"two\n", Some(first.version.clone()))
        .await
        .unwrap();
    let second = read_at(&path).await.unwrap();
    assert_eq!(second.bytes, b"two\n");
    assert_eq!(second.version, written);
    assert_ne!(second.version, first.version);
    // Nothing is left beside it.
    assert_eq!(dir.names(), ["a.txt"]);
}

#[tokio::test]
async fn what_isnt_a_regular_file_isnt_read() {
    let dir = Dir::new();
    assert_eq!(read_at(&dir.path("none")).await, Err(FileError::NotFound));
    assert_eq!(
        read_at(&dir.path("none/deeper")).await,
        Err(FileError::NotFound)
    );
    assert_eq!(read_at(&dir.0).await, Err(FileError::IsDirectory));
    // A FIFO with no writer would hold a read up forever.
    let fifo = dir.path("fifo");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    assert_eq!(read_at(&fifo).await, Err(FileError::NotRegular));
    assert_eq!(
        write_at(&fifo, b"x", None).await,
        Err(FileError::NotRegular)
    );
    assert_eq!(
        read_at(Path::new("/dev/null")).await,
        Err(FileError::NotRegular)
    );
}

#[tokio::test]
async fn a_file_too_big_isnt_read() {
    let dir = Dir::new();
    let path = dir.path("big");
    // Sparse: nothing is written.
    File::create(&path).unwrap().set_len(MOST_READ + 1).unwrap();
    assert_eq!(
        read_at(&path).await,
        Err(FileError::TooBig {
            size: MOST_READ + 1
        })
    );
}

#[tokio::test]
async fn a_write_expecting_another_version_changes_nothing() {
    let dir = Dir::new();
    let path = dir.path("a.txt");
    std::fs::write(&path, b"one").unwrap();
    let read = read_at(&path).await.unwrap();
    std::fs::write(&path, b"other").unwrap();
    assert_eq!(
        write_at(&path, b"mine", Some(read.version)).await,
        Err(FileError::Changed)
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"other");
    // Expecting none, of a file that's there.
    assert_eq!(
        write_at(&path, b"mine", None).await,
        Err(FileError::Changed)
    );
    // Expecting one, of a file that's gone.
    let gone = dir.path("gone");
    std::fs::write(&gone, b"x").unwrap();
    let read = read_at(&gone).await.unwrap();
    std::fs::remove_file(&gone).unwrap();
    assert_eq!(
        write_at(&gone, b"x", Some(read.version)).await,
        Err(FileError::Changed)
    );
    assert!(!gone.exists());
    assert_eq!(dir.names(), ["a.txt"]);
}

#[tokio::test]
async fn a_new_file_is_made_with_its_folders() {
    let dir = Dir::new();
    let path = dir.path("new/deeper/a.txt");
    let version = write_at(&path, b"hi", None).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"hi");
    assert_eq!(read_at(&path).await.unwrap().version, version);
}

#[tokio::test]
async fn a_write_keeps_the_files_mode() {
    let dir = Dir::new();
    let path = dir.path("run.sh");
    std::fs::write(&path, b"echo").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o741)).unwrap();
    let read = read_at(&path).await.unwrap();
    write_at(&path, b"echo hi", Some(read.version))
        .await
        .unwrap();
    let mode = std::fs::metadata(&path).unwrap().mode() & 0o7777;
    assert_eq!(mode, 0o741);
}

#[tokio::test]
async fn a_symlink_is_followed_and_what_it_leads_to_written() {
    let dir = Dir::new();
    let target = dir.path("target.txt");
    let link = dir.path("link.txt");
    std::fs::write(&target, b"one").unwrap();
    std::os::unix::fs::symlink("target.txt", &link).unwrap();
    let read = read_at(&link).await.unwrap();
    assert_eq!(read.bytes, b"one");
    write_at(&link, b"two", Some(read.version)).await.unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"two");
    // A link to a file not there yet makes it.
    let dangling = dir.path("dangling");
    std::os::unix::fs::symlink("made.txt", &dangling).unwrap();
    write_at(&dangling, b"new", None).await.unwrap();
    assert_eq!(std::fs::read(dir.path("made.txt")).unwrap(), b"new");
    assert!(
        std::fs::symlink_metadata(&dangling)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[tokio::test]
async fn a_hard_linked_file_is_written_in_place_keeping_its_links() {
    let dir = Dir::new();
    let path = dir.path("a.txt");
    let other = dir.path("b.txt");
    std::fs::write(&path, b"one").unwrap();
    std::fs::hard_link(&path, &other).unwrap();
    let inode = std::fs::metadata(&path).unwrap().ino();
    let read = read_at(&path).await.unwrap();
    let version = write_at(&path, b"two, longer", Some(read.version))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&other).unwrap(), b"two, longer");
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(read_at(&other).await.unwrap().version, version);
}

#[tokio::test]
async fn a_replaced_file_is_a_new_inode_written_whole() {
    let dir = Dir::new();
    let path = dir.path("a.txt");
    std::fs::write(&path, b"one").unwrap();
    let inode = std::fs::metadata(&path).unwrap().ino();
    let read = read_at(&path).await.unwrap();
    write_at(&path, b"two", Some(read.version)).await.unwrap();
    // Atomic: the old one was never written to.
    assert_ne!(std::fs::metadata(&path).unwrap().ino(), inode);
}

#[test]
fn errors_say_what_they_mean_for_a_tool() {
    let path = Path::new("/x");
    let of = |errno| io_error(path, &io::Error::from_raw_os_error(errno));
    assert_eq!(of(libc::ENOENT), FileError::NotFound);
    assert!(matches!(of(libc::EACCES), FileError::Denied(why) if why.starts_with("/x: ")));
    assert!(matches!(of(libc::EIO), FileError::Other(_)));
}
