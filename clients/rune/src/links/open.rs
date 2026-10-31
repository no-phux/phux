//! Whether a `file://` link may open here: GPUI's rules. Output is untrusted,
//! so a link naming another machine, an app or a program stays closed.

use std::path::{Component, Path, PathBuf};

/// How many symlinks one click follows. Linux's `MAXSYMLINKS`.
const MAX_SYMLINKS: usize = 40;
const ANOTHER_MACHINE: &str = "That file link names another machine, so it wasn't opened.";
const PROGRAM: &str = "That file is an app or a program, so it wasn't opened.";
const BAD_LINK: &str = "That file link wasn't opened.";

/// Why `url` must not open; `None` when it may.
pub(super) fn check_file(url: &str) -> Option<String> {
    let rest = url.get("file://".len()..)?;
    let (authority, raw) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        None => (rest, "/"),
    };
    let Some(authority) = percent_decode(authority) else {
        return Some(BAD_LINK.to_owned());
    };
    if !(authority.is_empty() || authority.eq_ignore_ascii_case("localhost")) {
        return Some(ANOTHER_MACHINE.to_owned());
    }
    let Some(path) = decoded_path(raw) else {
        return Some(BAD_LINK.to_owned());
    };
    launch_block(Path::new(&path)).err().map(str::to_owned)
}

/// The absolute path, without its query and fragment.
fn decoded_path(raw: &str) -> Option<String> {
    let bare = raw.split(['?', '#']).next().unwrap_or(raw);
    let decoded = percent_decode(bare)?;
    (!decoded.contains('\0') && decoded.starts_with('/')).then_some(decoded)
}

fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The refusal when `path`, followed through its symlinks, names another
/// machine or something that runs.
fn launch_block(path: &Path) -> Result<(), &'static str> {
    if names_share(path) {
        return Err(ANOTHER_MACHINE);
    }
    let mut current = normal(path);
    for _ in 0..MAX_SYMLINKS {
        check_lexical(&current)?;
        let Ok(meta) = std::fs::symlink_metadata(&current) else {
            return Ok(());
        };
        if !meta.file_type().is_symlink() {
            return if is_executable(&meta) {
                Err(PROGRAM)
            } else {
                Ok(())
            };
        }
        let target = std::fs::read_link(&current).map_err(|_| PROGRAM)?;
        if names_share(&target) {
            return Err(ANOTHER_MACHINE);
        }
        let parent = current.parent().ok_or(PROGRAM)?;
        current = normal(&parent.join(target));
    }
    Err(PROGRAM)
}

/// What a path says about itself, before any stat could reach a mount.
fn check_lexical(path: &Path) -> Result<(), &'static str> {
    let automount = path
        .components()
        .nth(1)
        .is_some_and(|part| part.as_os_str() == "net");
    if automount {
        return Err(ANOTHER_MACHINE);
    }
    if is_bundle(path) || runs_by_extension(path) {
        return Err(PROGRAM);
    }
    Ok(())
}

fn names_share(path: &Path) -> bool {
    path.to_string_lossy().starts_with("//")
}

fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn is_bundle(path: &Path) -> bool {
    path.components().any(|part| {
        let name = part.as_os_str().to_string_lossy().to_ascii_lowercase();
        name.len() > ".app".len() && name.ends_with(".app")
    })
}

fn runs_by_extension(path: &Path) -> bool {
    let extension = path.extension().and_then(|extension| extension.to_str());
    extension.is_some_and(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "command" | "desktop" | "exe" | "bat" | "cmd"
        )
    })
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.is_file() && meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_machines_and_programs_stay_closed() {
        assert_eq!(
            check_file("file://server/share/a.txt").as_deref(),
            Some(ANOTHER_MACHINE)
        );
        assert_eq!(
            check_file("file:////server/share").as_deref(),
            Some(ANOTHER_MACHINE)
        );
        assert_eq!(
            check_file("file:///net/box/a.txt").as_deref(),
            Some(ANOTHER_MACHINE)
        );
        assert_eq!(
            check_file("file:///Applications/Mail.app").as_deref(),
            Some(PROGRAM)
        );
        assert_eq!(
            check_file("file:///tmp/nothing-here/run%2Ecommand").as_deref(),
            Some(PROGRAM)
        );
        assert_eq!(check_file("file:///bin/sh").as_deref(), Some(PROGRAM));
    }

    #[test]
    fn plain_local_files_open() {
        assert_eq!(check_file("file:///tmp/nothing-here/notes.txt"), None);
        assert_eq!(check_file("file://localhost/tmp/nothing-here/a b"), None);
        assert_eq!(check_file("file:///tmp/nothing-here/a.txt?x#y"), None);
        assert_eq!(check_file("file:///%ZZ").as_deref(), Some(BAD_LINK));
    }
}
