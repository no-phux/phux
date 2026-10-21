//! Checkout-relative paths, and opening them without following a symlink.
//!
//! A path is checked lexically first: plain components only, none naming
//! `.git` under any spelling a case-insensitive or Unicode-ignoring
//! filesystem resolves to it (Git's `is_hfs_dotgit` / `is_ntfs_dotgit`).
//! Files and directories are then opened one component at a time with
//! `openat(O_NOFOLLOW)` from the checkout's top level, so a component
//! swapped for a symlink after a check cannot redirect the read.

use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Component, Path};

use super::types::Result;
use super::{CheckoutError, MAX_PATH_BYTES, MAX_PATHS};

const DIR_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
/// `O_NONBLOCK` so a FIFO opens instead of waiting for a writer; the caller
/// then refuses anything but a regular file.
const FILE_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;

/// A checkout-relative path: non-empty, plain components only, nothing that
/// names `.git`.
pub fn relative(path: &str) -> Result<&str> {
    let invalid = |why: &str| CheckoutError::InvalidPath(format!("{path:?} {why}"));
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains('\0') {
        return Err(invalid("is not a path inside this checkout"));
    }
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) if is_dotgit(name) => {
                return Err(invalid("is inside .git"));
            }
            Component::Normal(_) => {}
            _ => return Err(invalid("is not a plain path inside this checkout")),
        }
    }
    Ok(path)
}

pub(super) fn path_list(paths: &[String]) -> Result<Vec<&str>> {
    if paths.is_empty() || paths.len() > MAX_PATHS {
        return Err(CheckoutError::InvalidRequest(format!(
            "name between 1 and {MAX_PATHS} paths"
        )));
    }
    paths.iter().map(|path| relative(path)).collect()
}

/// Whether `name` resolves to `.git` on some filesystem: ASCII case folded,
/// the code points HFS+ ignores removed, and the trailing dots and spaces
/// NTFS drops trimmed; `git~1` is NTFS's short name for it.
pub(super) fn is_dotgit(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let folded: String = name
        .chars()
        .filter(|c| !is_hfs_ignorable(*c))
        .collect::<String>()
        .to_ascii_lowercase();
    let trimmed = folded.trim_end_matches(['.', ' ']);
    trimmed == ".git" || trimmed == "git~1"
}

/// The code points HFS+ drops when comparing names (Git's `next_hfs_char`).
const fn is_hfs_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'
    )
}

/// Why a walk beneath the top level stopped.
#[derive(Debug)]
pub(super) enum Walk {
    /// A component is missing.
    NotFound,
    /// A component is a symbolic link.
    Symlink,
    /// Anything else, from the OS.
    Io(io::Error),
}

impl Walk {
    pub(super) fn into_error(self, rel: &str) -> CheckoutError {
        match self {
            Self::NotFound => CheckoutError::NotFound(format!("{rel} does not exist")),
            Self::Symlink => CheckoutError::InvalidPath(format!("{rel} crosses a symbolic link")),
            Self::Io(error) => io_error(&format!("Could not open {rel}"), &error),
        }
    }
}

/// Open the regular-file candidate `rel` beneath `root`.
pub(super) fn open_file(root: &Path, rel: &str) -> std::result::Result<File, Walk> {
    walk(root, rel, FILE_FLAGS).map(File::from)
}

/// Open the directory `rel` (empty for `root` itself) beneath `root`.
pub(super) fn open_dir(root: &Path, rel: &str) -> std::result::Result<OwnedFd, Walk> {
    walk(root, rel, DIR_FLAGS)
}

fn walk(root: &Path, rel: &str, last: libc::c_int) -> std::result::Result<OwnedFd, Walk> {
    let mut dir: OwnedFd = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(root)
        .map_err(Walk::Io)?
        .into();
    let names: Vec<&OsStr> = Path::new(rel)
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    for (index, name) in names.iter().enumerate() {
        let flags = if index + 1 == names.len() {
            last
        } else {
            DIR_FLAGS
        };
        let name = CString::new(name.as_bytes()).map_err(|_| Walk::NotFound)?;
        dir = openat(&dir, &name, flags).map_err(|error| classify(&dir, &name, error))?;
    }
    Ok(dir)
}

fn openat(dir: &OwnedFd, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: `name` is NUL-terminated and `dir` is an open descriptor.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh descriptor this process owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn lstat_at(dir: &OwnedFd, name: &CStr) -> io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` is written by a successful `fstatat` before it is read.
    let status = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstatat` succeeded and filled `stat`.
    Ok(unsafe { stat.assume_init() })
}

/// Name an `openat` failure: a symlink and a missing component read better
/// than `ELOOP` and `ENOENT`.
fn classify(dir: &OwnedFd, name: &CStr, error: io::Error) -> Walk {
    match lstat_at(dir, name) {
        Ok(stat) if stat.st_mode & libc::S_IFMT == libc::S_IFLNK => Walk::Symlink,
        Err(missing) if missing.kind() == io::ErrorKind::NotFound => Walk::NotFound,
        _ if error.kind() == io::ErrorKind::NotFound => Walk::NotFound,
        _ => Walk::Io(error),
    }
}

/// One directory entry, read through the directory's descriptor.
#[derive(Debug)]
pub(super) struct Found {
    pub(super) name: String,
    pub(super) symlink: bool,
    pub(super) directory: bool,
    pub(super) size: u64,
}

/// Up to `limit` UTF-8-named entries of the open directory `dir`, without
/// `.`, `..`, or `.git`; the flag says more were left unread.
pub(super) fn list(dir: OwnedFd, limit: usize) -> io::Result<(Vec<Found>, bool)> {
    let stat_dir = dir.try_clone()?;
    let raw = dir.into_raw_fd();
    // SAFETY: `raw` is an open directory descriptor whose ownership passes
    // to the stream; `closedir` below releases both.
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so `raw` is still ours to close.
        unsafe { libc::close(raw) };
        return Err(error);
    }
    let mut found = Vec::new();
    let mut truncated = false;
    loop {
        // SAFETY: `stream` is a live directory stream.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `readdir` returned a valid entry whose name is
        // NUL-terminated; it is copied before the next call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_owned();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." || is_dotgit(OsStr::from_bytes(bytes)) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            continue;
        };
        if found.len() >= limit {
            truncated = true;
            break;
        }
        let Ok(stat) = lstat_at(&stat_dir, &name) else {
            continue;
        };
        let kind = stat.st_mode & libc::S_IFMT;
        found.push(Found {
            name: text.to_owned(),
            symlink: kind == libc::S_IFLNK,
            directory: kind == libc::S_IFDIR,
            size: u64::try_from(stat.st_size).unwrap_or(0),
        });
    }
    // SAFETY: `stream` is live and closed exactly once.
    unsafe { libc::closedir(stream) };
    Ok((found, truncated))
}

pub(super) fn io_error(what: &str, error: &io::Error) -> CheckoutError {
    if error.kind() == io::ErrorKind::NotFound {
        CheckoutError::NotFound(format!("{what}: {error}"))
    } else {
        CheckoutError::Io(format!("{what}: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_dotgit_is_refused() {
        for name in [
            ".git",
            ".GIT",
            ".Git.",
            ".git ",
            ".git. .",
            "git~1",
            "GIT~1",
            ".g\u{200c}it",
            "\u{feff}.git",
            ".gi\u{206f}t",
        ] {
            assert!(is_dotgit(OsStr::new(name)), "{name:?}");
            assert!(relative(&format!("src/{name}/config")).is_err(), "{name:?}");
        }
        for name in [".github", ".gitignore", "git", ".gitx", "dotgit"] {
            assert!(!is_dotgit(OsStr::new(name)), "{name:?}");
        }
    }

    #[test]
    fn a_swapped_in_symlink_is_never_followed() {
        let root = std::env::temp_dir().join(format!("phux-beneath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real/file"), "inside").unwrap();
        let outside = root.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("file"), "outside").unwrap();
        assert!(open_file(&root, "real/file").is_ok());

        // The check-then-open race, collapsed: the directory is already a
        // symlink when the walk reaches it.
        std::fs::remove_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("real")).unwrap();
        assert!(matches!(open_file(&root, "real/file"), Err(Walk::Symlink)));
        assert!(matches!(open_dir(&root, "real"), Err(Walk::Symlink)));
        assert!(matches!(open_file(&root, "nope"), Err(Walk::NotFound)));
    }
}
