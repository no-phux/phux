//! A screenshot taken with the system's own region picker: `screencapture`
//! on macOS; `slurp` and `grim` on Wayland, `maim`, `gnome-screenshot` or
//! `spectacle` elsewhere on Linux. The picker owns the screen until the user
//! picks a region or presses Escape, so `take` blocks: run it off the UI thread.

use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::{Command, Stdio};

/// How a capture ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shot {
    /// The picked region as PNG bytes.
    Taken(Vec<u8>),
    /// The user pressed Escape or clicked away.
    Cancelled,
    /// Why there is no screenshot.
    Failed(String),
}

/// Why a screenshot can't be taken here, before trying. On macOS a picker
/// started without Screen Recording sees only the wallpaper, so ask for it.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn refusal() -> Option<&'static str> {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    // Safety: documented calls with no arguments; the request shows the
    // system's prompt once and returns.
    unsafe {
        if CGPreflightScreenCaptureAccess() {
            return None;
        }
        CGRequestScreenCaptureAccess();
    }
    Some(
        "Screenshots need Screen Recording: allow Phux Rune in System Settings › \
         Privacy & Security, then try again.",
    )
}

#[cfg(target_os = "linux")]
pub fn refusal() -> Option<&'static str> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    tool(wayland, on_path).is_none().then_some(NO_TOOL)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn refusal() -> Option<&'static str> {
    Some("Screenshots aren't available on this device.")
}

#[cfg(target_os = "linux")]
const NO_TOOL: &str = "No screenshot tool found: install slurp and grim (Wayland) or maim (X11).";

/// Pick a region and return it. Blocks until the picker closes.
pub fn take() -> Shot {
    let dir = match scratch() {
        Ok(dir) => dir,
        Err(error) => return Shot::Failed(format!("couldn't make a folder for it: {error}")),
    };
    let shot = capture(&dir.join("shot.png"));
    let _ = std::fs::remove_dir_all(&dir);
    shot
}

/// A new folder only this user can read, for the picker to write into: a
/// shared temp folder must not hand anyone else the name first.
fn scratch() -> std::io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("phux-shot-{}-{nanos}", std::process::id()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir)?;
    Ok(dir)
}

#[cfg(target_os = "macos")]
fn capture(file: &Path) -> Shot {
    // -i: pick a region (Space picks a window); -x: no shutter sound.
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-i", "-x", "-t", "png"])
        .arg(file)
        .stdin(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => read(file),
        Ok(status) => Shot::Failed(format!("screencapture ended with {status}")),
        Err(error) => Shot::Failed(format!("couldn't start screencapture: {error}")),
    }
}

#[cfg(target_os = "linux")]
fn capture(file: &Path) -> Shot {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let Some(tool) = tool(wayland, on_path) else {
        return Shot::Failed(NO_TOOL.to_owned());
    };
    let picked = match tool {
        Tool::Grim => grim(file),
        Tool::Maim => run(Command::new("maim").arg("-s").arg(file)),
        Tool::Gnome => run(Command::new("gnome-screenshot")
            .arg("-a")
            .arg("-f")
            .arg(file)),
        Tool::Spectacle => run(Command::new("spectacle")
            .args(["-b", "-n", "-r", "-o"])
            .arg(file)),
    };
    match picked {
        // The pickers end unsuccessfully, or write nothing, when cancelled.
        Ok(_) => read(file),
        Err(error) => Shot::Failed(format!("couldn't start {}: {error}", tool.name())),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn capture(_: &Path) -> Shot {
    Shot::Failed("screenshots aren't available on this device".to_owned())
}

/// What the picker wrote; nothing written means it was cancelled.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux", test)),
    allow(dead_code)
)]
fn read(file: &Path) -> Shot {
    match std::fs::read(file) {
        Ok(png) if !png.is_empty() => Shot::Taken(png),
        Ok(_) => Shot::Cancelled,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Shot::Cancelled,
        Err(error) => Shot::Failed(format!("couldn't read the screenshot: {error}")),
    }
}

/// A Linux region picker, in the order tried.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    /// `slurp` picks the region, `grim` grabs it (wlroots, Hyprland, KDE).
    Grim,
    Maim,
    Gnome,
    Spectacle,
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
impl Tool {
    const fn name(self) -> &'static str {
        match self {
            Self::Grim => "grim",
            Self::Maim => "maim",
            Self::Gnome => "gnome-screenshot",
            Self::Spectacle => "spectacle",
        }
    }
}

/// The first picker installed: `grim` only under Wayland, `maim` only
/// under X11, and the desktops' own tools under either.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn tool(wayland: bool, installed: impl Fn(&str) -> bool) -> Option<Tool> {
    let order: &[Tool] = if wayland {
        &[Tool::Grim, Tool::Gnome, Tool::Spectacle]
    } else {
        &[Tool::Maim, Tool::Gnome, Tool::Spectacle]
    };
    order.iter().copied().find(|tool| match tool {
        Tool::Grim => installed("grim") && installed("slurp"),
        other => installed(other.name()),
    })
}

#[cfg(target_os = "linux")]
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
}

#[cfg(target_os = "linux")]
fn run(command: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    command.stdin(Stdio::null()).stdout(Stdio::null()).status()
}

/// `slurp` prints the picked region; a cancel ends it unsuccessfully.
#[cfg(target_os = "linux")]
fn grim(file: &Path) -> std::io::Result<std::process::ExitStatus> {
    let picked = Command::new("slurp")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !picked.status.success() {
        return Ok(picked.status);
    }
    let region = String::from_utf8_lossy(&picked.stdout).trim().to_owned();
    run(Command::new("grim").arg("-g").arg(region).arg(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grim_needs_slurp_and_wayland() {
        let all = |_: &str| true;
        assert_eq!(tool(true, all), Some(Tool::Grim));
        assert_eq!(tool(false, all), Some(Tool::Maim));
        let no_slurp = |name: &str| name != "slurp";
        assert_eq!(tool(true, no_slurp), Some(Tool::Gnome));
    }

    #[test]
    fn the_desktops_tools_are_the_fallback() {
        let only = |name: &'static str| move |found: &str| found == name;
        assert_eq!(tool(false, only("spectacle")), Some(Tool::Spectacle));
        assert_eq!(tool(true, only("gnome-screenshot")), Some(Tool::Gnome));
        assert_eq!(tool(true, only("maim")), None);
        assert_eq!(tool(false, |_| false), None);
    }

    #[test]
    fn nothing_written_is_a_cancel() {
        let dir = scratch().unwrap();
        let file = dir.join("shot.png");
        assert_eq!(read(&file), Shot::Cancelled);
        std::fs::write(&file, b"").unwrap();
        assert_eq!(read(&file), Shot::Cancelled);
        std::fs::write(&file, b"\x89PNG").unwrap();
        assert_eq!(read(&file), Shot::Taken(b"\x89PNG".to_vec()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_scratch_folder_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch().unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(mode & 0o777, 0o700);
    }
}
