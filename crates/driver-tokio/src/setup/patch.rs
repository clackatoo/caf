//! Patching an install as its last step, and preparing a new patch level
//! from an earlier install: the setup driver's work on files.
//!
//! Nothing here changes a file in place. A patched file is written whole
//! beside the original and renamed over it, the original kept under
//! [`ORIGINAL_SUFFIX`]; a prepared install is a new folder of links to an
//! earlier one's files, whose names are what's changed. So an earlier
//! install, and anything running from it, never sees a byte change.

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use caf_core::agent::InstallFailure;
use caf_core::install::{FilePatch, ORIGINAL_SUFFIX, Patch, PatchError};

/// What a file being patched is written as, beside it, before it's
/// renamed into place.
const PATCHING_SUFFIX: &str = ".caf-patching";

/// Patches the install being built in `build` with `patch`, file by file,
/// in order. Any failure leaves `build` half-patched: it's then removed,
/// never put in place.
pub(super) fn patch_install(build: &Path, patch: &Patch) -> Result<(), InstallFailure> {
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
    let original = fs::read(&place).map_err(io_failure)?;
    let patched = file.apply(&original).map_err(|error| match error {
        PatchError::NotAsPinned => refused("isn't the file this patch was written for"),
        PatchError::Found { edit, times: 0 } => {
            refused(&format!("doesn't hold the text of edit {edit}"))
        }
        PatchError::Found { edit, times } => refused(&format!(
            "holds the text of edit {edit} {times} times, not once"
        )),
        PatchError::NotAsTested => refused("patched isn't the file this patch was tested to make"),
        // No other kind is given yet.
        _ => refused("couldn't be patched"),
    })?;
    // The patched file keeps the original's permissions: a script stays
    // runnable.
    let mode = fs::metadata(&place)
        .map_err(io_failure)?
        .permissions()
        .mode();
    let temp = beside(&place, PATCHING_SUFFIX);
    let written = (|| {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        out.write_all(&patched)?;
        out.set_permissions(fs::Permissions::from_mode(mode & 0o7777))?;
        out.sync_all()?;
        fs::rename(&place, beside(&place, ORIGINAL_SUFFIX))?;
        fs::rename(&temp, &place)
    })();
    written.map_err(io_failure)
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

/// Makes `build` as the complete install `from` is, as the start of a new
/// patch level: each file a hard link to `from`'s, or a copy where it
/// can't be one, each link and folder the same, but for `from`'s `.used`
/// file, which is its own. Then each file `from` patched is put back as
/// npm installed it, its original renamed over the patched copy. Returns
/// `false` if `stopped` first.
pub(super) fn link_earlier(from: &Path, build: &Path, stopped: &AtomicBool) -> io::Result<bool> {
    let mut originals = Vec::new();
    if !link_tree(from, build, true, &mut originals, stopped)? {
        return Ok(false);
    }
    for original in originals {
        let patched = original.with_file_name(
            (original.file_name().and_then(|name| name.to_str()))
                .and_then(|name| name.strip_suffix(ORIGINAL_SUFFIX))
                .unwrap_or_default(),
        );
        fs::rename(&original, &patched)?;
    }
    Ok(true)
}

/// Fills the folder `to`, already made, as `from` is (see
/// [`link_earlier`]), noting in `originals` every original of a patched
/// file it links: `top` while it's the install's own folder. Anything but
/// files, folders and links, such as a pipe, is left out. Returns `false`
/// if `stopped` first.
fn link_tree(
    from: &Path,
    to: &Path,
    top: bool,
    originals: &mut Vec<PathBuf>,
    stopped: &AtomicBool,
) -> io::Result<bool> {
    for entry in fs::read_dir(from)? {
        if stopped.load(Ordering::Relaxed) {
            return Ok(false);
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
            if !link_tree(&source, &target, false, originals, stopped)? {
                return Ok(false);
            }
            // Only once filled, as it may not be writable.
            fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o7777))?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&source)?, &target)?;
        } else if kind.is_file() {
            if fs::hard_link(&source, &target).is_err() {
                fs::copy(&source, &target)?;
                File::open(&target)?.sync_all()?;
            }
            if name.to_str().is_some_and(|name| {
                name.len() > ORIGINAL_SUFFIX.len() && name.ends_with(ORIGINAL_SUFFIX)
            }) {
                originals.push(target);
            }
        }
    }
    Ok(!stopped.load(Ordering::Relaxed))
}

/// An install failure for an error on the disk while patching.
fn io_failure(error: io::Error) -> InstallFailure {
    match error.raw_os_error() {
        Some(libc::ENOSPC | libc::EDQUOT) => InstallFailure::NoSpace,
        _ => InstallFailure::Other(format!("patching it failed: {error}")),
    }
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
}
