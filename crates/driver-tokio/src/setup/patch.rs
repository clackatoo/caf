//! Patching an install as its last step, and preparing a new patch level
//! from an earlier install: the setup driver's work on files.
//!
//! Nothing here changes a file in place. A patched file is written whole
//! beside the original and renamed over it, the original kept under
//! [`ORIGINAL_SUFFIX`]; a prepared install is a new folder of links to an
//! earlier one's files, whose names are what's changed. So an earlier
//! install, and anything running from it, never sees a byte change.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use caf_core::agent::InstallFailure;
use caf_core::install::{FilePatch, ORIGINAL_SUFFIX, Patch, PatchError};

use super::install::failure;

/// What a file being patched is written as, beside it, before it's
/// renamed into place.
const PATCHING_SUFFIX: &str = ".caf-patching";

/// What the names of the files CAF keeps beside a patched one hold: no
/// file it patches may be named so.
const OWN_NAMES: &str = ".caf-";

/// Patches the install being built in `build` with `patch`, file by file,
/// in order. Any failure leaves `build` half-patched: it's then removed,
/// never put in place. A file named as CAF names its own, or patched
/// twice, refuses the whole patch before anything is written.
pub(super) fn patch_install(build: &Path, patch: &Patch) -> Result<(), InstallFailure> {
    let mut seen = HashSet::new();
    for file in &patch.files {
        let path = file.path.as_str();
        let name = path.rsplit('/').next().unwrap_or(path);
        if name.contains(OWN_NAMES) {
            return Err(InstallFailure::Patch(format!(
                "{path} is named as CAF names its own files"
            )));
        }
        if !seen.insert(path) {
            return Err(InstallFailure::Patch(format!("{path} is patched twice")));
        }
    }
    patch
        .files
        .iter()
        .try_for_each(|file| patch_file(build, file))
}

/// Patches one file of the install in `build`: it's read, patched, written
/// whole beside it and synced, the original renamed aside, and the patched
/// file renamed into its place.
fn patch_file(build: &Path, file: &FilePatch) -> Result<(), InstallFailure> {
    let path = file.path.as_str();
    let refused = |why: &str| InstallFailure::Patch(format!("{path} {why}"));
    let place = file_in(build, path).map_err(refused)?;
    let original = fs::read(&place).map_err(failure)?;
    let patched = file.apply(&original).map_err(|error| match error {
        PatchError::NotAsPinned => refused("isn't the file this patch was written for"),
        PatchError::Found { edit, times: 0 } => {
            refused(&format!("doesn't hold the text of edit {edit}"))
        }
        PatchError::Found { edit, times } => refused(&format!(
            "holds the text of edit {edit} {times} times, not once"
        )),
        PatchError::NotAsTested => refused("patched isn't the file this patch was tested to make"),
        PatchError::NothingToFind { edit } => {
            refused(&format!("is given no text to find by edit {edit}"))
        }
        // No other kind is given yet.
        _ => refused("couldn't be patched"),
    })?;
    // The patched file keeps the original's permissions: a script stays
    // runnable.
    let mode = fs::metadata(&place).map_err(failure)?.permissions().mode();
    let temp = beside(&place, PATCHING_SUFFIX);
    let written = (|| {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        out.write_all(&patched)?;
        out.set_permissions(fs::Permissions::from_mode(mode & 0o7777))?;
        out.sync_all()
    })();
    written.map_err(failure)?;
    set_aside(&place, &beside(&place, ORIGINAL_SUFFIX)).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            refused("has a file named as its original beside it already")
        } else {
            failure(error)
        }
    })?;
    fs::rename(&temp, &place).map_err(failure)
}

/// Renames `place` to `aside`, never over anything there: if something
/// is, it fails with [`io::ErrorKind::AlreadyExists`], and `place` stays.
/// Only in CAF's hidden build folder, under the installs' lock, where
/// nothing else writes.
fn set_aside(place: &Path, aside: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;
    match renameat_with(CWD, place, CWD, aside, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        // A file system that can't rename so.
        Err(error)
            if [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP, Errno::OPNOTSUPP].contains(&error) =>
        {
            link_aside(place, aside)
        }
        Err(error) => Err(error.into()),
    }
}

/// As [`set_aside`], with a hard link, which is made only where nothing
/// is: `place` keeps its name too, until the patched file is renamed over
/// it. On a file system that can't make one either, as
/// [`rename_if_free`].
fn link_aside(place: &Path, aside: &Path) -> io::Result<()> {
    match fs::hard_link(place, aside) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => rename_if_free(place, aside),
        linked => linked,
    }
}

/// As [`set_aside`], by looking that nothing is at `aside`, then renaming:
/// the last resort, safe only as nothing else writes in the build folder.
fn rename_if_free(place: &Path, aside: &Path) -> io::Result<()> {
    match fs::symlink_metadata(aside) {
        Ok(_) => Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::rename(place, aside),
        Err(error) => Err(error),
    }
}

/// The file `path` names in the install in `build`, a regular file inside
/// it: `path` is relative, with `/` between its parts, none of them empty,
/// `.` or `..`, and no part of the way there is a link, which could lead
/// outside. Otherwise, why not.
fn file_in(build: &Path, path: &str) -> Result<PathBuf, &'static str> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == ".." || part.contains('\0'))
    {
        return Err("isn't a path inside the install");
    }
    let mut place = build.to_owned();
    for (n, part) in parts.iter().enumerate() {
        place.push(part);
        let last = n + 1 == parts.len();
        match fs::symlink_metadata(&place) {
            Ok(found) if found.file_type().is_symlink() => {
                return Err("is reached through a link");
            }
            Ok(found) if last && found.is_file() => {}
            Ok(found) if !last && found.is_dir() => {}
            Ok(_) if last => return Err("isn't a file"),
            Ok(_) => return Err("is reached through something that isn't a folder"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err("isn't in the install");
            }
            Err(_) => return Err("can't be looked at"),
        }
    }
    Ok(place)
}

/// `path` with `suffix` added to its name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// How an earlier install is linked. Only tests change it.
#[derive(Clone, Copy, Default)]
pub(super) struct Linking {
    /// How long linking each entry takes, as well, so a test can stop it
    /// meanwhile.
    pub(super) pause: Duration,
    /// Every file is copied, as where hard links can't be made.
    pub(super) copy: bool,
}

/// What linking an earlier install left to do once the new one is
/// patched: each folder's own permissions, deepest first, which it went
/// without meanwhile, writable by the user, so the patch can write in it,
/// and anything left of a failed one can be removed.
pub(super) struct Linked {
    modes: Vec<(PathBuf, u32)>,
}

impl Linked {
    /// Gives each folder its own permissions back.
    pub(super) fn finish(self) -> io::Result<()> {
        for (folder, mode) in self.modes {
            fs::set_permissions(folder, fs::Permissions::from_mode(mode & 0o7777))?;
        }
        Ok(())
    }
}

/// What [`link_tree`] has made so far.
#[derive(Default)]
struct Tree {
    /// Each original of a patched file, and the patched file's name.
    originals: Vec<(PathBuf, PathBuf)>,
    /// Each folder's own permissions, deepest first.
    modes: Vec<(PathBuf, u32)>,
}

/// Makes `build` as the complete install `from` is, as the start of a new
/// patch level: each file a hard link to `from`'s, or a copy where it
/// can't be one, each link and folder the same, but for `from`'s `.used`
/// file, which is its own. Then each file `from` patched is put back as
/// npm installed it, its original renamed over the patched copy. `None` if
/// `stopped` first.
pub(super) fn link_earlier(
    from: &Path,
    build: &Path,
    linking: Linking,
    stopped: &AtomicBool,
) -> io::Result<Option<Linked>> {
    let mut tree = Tree::default();
    if !link_tree(from, build, true, linking, &mut tree, stopped)? {
        return Ok(None);
    }
    for (original, patched) in &tree.originals {
        fs::rename(original, patched)?;
    }
    Ok(Some(Linked { modes: tree.modes }))
}

/// Fills the folder `to`, already made, as `from` is (see
/// [`link_earlier`]), noting in `tree` what's left to do: `top` while it's
/// the install's own folder. Anything but files, folders and links, such
/// as a pipe, is left out. Returns `false` if `stopped` first.
fn link_tree(
    from: &Path,
    to: &Path,
    top: bool,
    linking: Linking,
    tree: &mut Tree,
    stopped: &AtomicBool,
) -> io::Result<bool> {
    for entry in fs::read_dir(from)? {
        if stopped.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if !linking.pause.is_zero() {
            std::thread::sleep(linking.pause);
        }
        let entry = entry?;
        let name = entry.file_name();
        if top && name == ".used" {
            continue;
        }
        let (source, target) = (entry.path(), to.join(&name));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let mode = entry.metadata()?.permissions().mode();
            fs::DirBuilder::new().mode(0o700).create(&target)?;
            if !link_tree(&source, &target, false, linking, tree, stopped)? {
                return Ok(false);
            }
            tree.modes.push((target, mode));
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&source)?, &target)?;
        } else if kind.is_file() {
            if linking.copy || fs::hard_link(&source, &target).is_err() {
                fs::copy(&source, &target)?;
                File::open(&target)?.sync_all()?;
            }
            let patched = (name.to_str())
                .and_then(|name| name.strip_suffix(ORIGINAL_SUFFIX))
                .filter(|patched| !patched.is_empty());
            if let Some(patched) = patched {
                tree.originals.push((target.clone(), to.join(patched)));
            }
        }
    }
    Ok(!stopped.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patched_file_is_named_by_a_plain_path_inside_the_install() {
        let build = std::env::temp_dir().join(format!("caf-patch-paths-{}", std::process::id()));
        let _ = fs::remove_dir_all(&build);
        fs::create_dir_all(build.join("dist")).unwrap();
        fs::write(build.join("dist/a.js"), "a").unwrap();
        std::os::unix::fs::symlink("dist", build.join("linked")).unwrap();
        std::os::unix::fs::symlink("a.js", build.join("dist/b.js")).unwrap();
        assert_eq!(file_in(&build, "dist/a.js"), Ok(build.join("dist/a.js")));
        for (path, why) in [
            ("", "isn't a path inside the install"),
            ("/etc/passwd", "isn't a path inside the install"),
            ("dist//a.js", "isn't a path inside the install"),
            ("./dist/a.js", "isn't a path inside the install"),
            ("dist/../dist/a.js", "isn't a path inside the install"),
            ("../a.js", "isn't a path inside the install"),
            ("dist/", "isn't a path inside the install"),
            ("linked/a.js", "is reached through a link"),
            ("dist/b.js", "is reached through a link"),
            ("dist", "isn't a file"),
            (
                "dist/a.js/x",
                "is reached through something that isn't a folder",
            ),
            ("dist/c.js", "isn't in the install"),
        ] {
            assert_eq!(file_in(&build, path), Err(why), "{path:?}");
        }
        fs::remove_dir_all(&build).unwrap();
    }

    #[test]
    fn an_original_is_set_aside_only_where_nothing_is() {
        let folder = std::env::temp_dir().join(format!("caf-patch-aside-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let (place, aside) = (folder.join("a.js"), folder.join("a.js.caf-original"));
        // Each way: the rename that never replaces, then, on a file
        // system that can't do that, a hard link, then, on one that can't
        // do that either, a rename of what's looked free.
        let ways: [fn(&Path, &Path) -> io::Result<()>; 3] = [set_aside, link_aside, rename_if_free];
        for (n, way) in ways.into_iter().enumerate() {
            fs::write(&place, "original").unwrap();
            let _ = fs::remove_file(&aside);
            way(&place, &aside).unwrap_or_else(|error| panic!("way {n}: {error}"));
            assert_eq!(fs::read(&aside).unwrap(), b"original", "way {n}");
            // With something there already, it's left as it was. (Both
            // made afresh: a hard link left them one file.)
            let _ = (fs::remove_file(&place), fs::remove_file(&aside));
            fs::write(&place, "original").unwrap();
            fs::write(&aside, "upstream's own").unwrap();
            let refused = way(&place, &aside).expect_err("something's there");
            assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists, "way {n}");
            assert_eq!(fs::read(&aside).unwrap(), b"upstream's own", "way {n}");
            assert_eq!(fs::read(&place).unwrap(), b"original", "way {n}");
        }
        fs::remove_dir_all(&folder).unwrap();
    }
}
