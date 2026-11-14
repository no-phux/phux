//! A screenshot taken with the system's own region picker: `screencapture`
//! on macOS; `slurp` and `grim` on Wayland, `maim`, `gnome-screenshot` or
//! `spectacle` elsewhere on Linux. The picker owns the screen until the user
//! picks a region or presses Escape, so `take` blocks: run it off the UI thread.

use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::{Command, Stdio};

mod picker;
pub(crate) use picker::Picker;

/// How a capture ended.
#[derive(Clone, PartialEq, Eq)]
pub enum Shot {
    /// The picked region as PNG bytes.
    Taken(Vec<u8>),
    /// The user pressed Escape or clicked away.
    Cancelled,
    /// Why there is no screenshot.
    Failed(String),
}

impl std::fmt::Debug for Shot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Taken(bytes) => f.debug_tuple("Taken").field(&bytes.len()).finish(),
            Self::Cancelled => f.write_str("Cancelled"),
            Self::Failed(why) => f.debug_tuple("Failed").field(why).finish(),
        }
    }
}
/// Why a screenshot can't be taken here, before trying. On macOS a picker
/// started without Screen Recording sees only the wallpaper, so ask for it.
#[cfg(all(target_os = "macos", not(test)))]
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

#[cfg(all(target_os = "linux", not(test)))]
pub fn refusal() -> Option<&'static str> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    tool(wayland, on_path).is_none().then_some(NO_TOOL)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn refusal() -> Option<&'static str> {
    Some("Screenshots aren't available on this device.")
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
pub fn refusal() -> Option<&'static str> {
    // Tests may drive a shortcut, but never own OS screen permission.
    Some("Screen capture is disabled in headless tests.")
}

#[cfg(target_os = "linux")]
const NO_TOOL: &str = "No screenshot tool found: install slurp and grim (Wayland) or maim (X11).";

/// Pick a region and return it. Blocks until the picker closes.
pub fn take(picker: &Picker) -> Shot {
    let dir = match scratch() {
        Ok(dir) => dir,
        Err(error) => return Shot::Failed(format!("couldn't make a folder for it: {error}")),
    };
    let cleanup = Cleanup(dir);
    let shot = capture(&cleanup.0.join("shot.png"), picker);
    match std::fs::remove_dir_all(&cleanup.0) {
        Ok(()) => shot,
        Err(_) => Shot::Failed("couldn't remove the private screenshot folder".into()),
    }
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
fn capture(file: &Path, picker: &Picker) -> Shot {
    // -i: pick a region (Space picks a window); -x: no shutter sound.
    picked(
        Command::new("/usr/sbin/screencapture")
            .args(["-i", "-x", "-t", "png"])
            .arg(file),
        file,
        picker,
        true,
    )
}

#[cfg(target_os = "linux")]
fn capture(file: &Path, picker: &Picker) -> Shot {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let Some(tool) = tool(wayland, on_path) else {
        return Shot::Failed(NO_TOOL.to_owned());
    };
    match tool {
        Tool::Grim => grim(file, picker),
        Tool::Maim => picked(Command::new("maim").arg("-s").arg(file), file, picker, true),
        Tool::Gnome => picked(
            Command::new("gnome-screenshot")
                .arg("-a")
                .arg("-f")
                .arg(file),
            file,
            picker,
            true,
        ),
        Tool::Spectacle => picked(
            Command::new("spectacle")
                .args(["-b", "-n", "-r", "-o"])
                .arg(file),
            file,
            picker,
            true,
        ),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn capture(_: &Path, _: &Picker) -> Shot {
    Shot::Failed("screenshots aren't available on this device".to_owned())
}

/// What the picker wrote; nothing written means it was cancelled.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux", test)),
    allow(dead_code)
)]
fn read(file: &Path) -> Shot {
    match bounded(file, phux_protocol::MAX_FILE_UPLOAD_SIZE) {
        Ok(png) if png.is_empty() => Shot::Cancelled,
        Ok(png) => match validate_png(&png) {
            Ok(()) => Shot::Taken(png),
            Err(_) => Shot::Failed("the picker did not write a complete, supported PNG".into()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Shot::Cancelled,
        Err(error) => Shot::Failed(format!("couldn't read the screenshot: {error}")),
    }
}

fn bounded(file: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    if !std::fs::symlink_metadata(file)?.is_file() {
        return Err(std::io::Error::other("screenshot is not a regular file"));
    }
    let file = std::fs::File::open(file)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(std::io::Error::other(
            "screenshot is not a regular file within its size limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > limit) {
        return Err(std::io::Error::other("screenshot exceeds its size limit"));
    }
    Ok(bytes)
}

fn validate_png(bytes: &[u8]) -> Result<(), ()> {
    let decoder = png::Decoder::new_with_limits(
        bytes,
        png::Limits {
            bytes: 64 * 1024 * 1024,
        },
    );
    let mut reader = decoder.read_info().map_err(|_| ())?;
    let info = reader.info();
    if info.width == 0 || info.height == 0 || info.width > 8192 || info.height > 8192 {
        return Err(());
    }
    let size = reader.output_buffer_size();
    if size > 256 * 1024 * 1024 {
        return Err(());
    }
    let mut pixels = vec![0; size];
    reader.next_frame(&mut pixels).map_err(|_| ())?;
    reader.finish().map_err(|_| ())
}

/// Exit 1 without diagnostics is the interactive tools' cancel convention.
/// Noninteractive failures and failures with diagnostics are never pictures.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn picked(command: &mut Command, file: &Path, picker: &Picker, interactive: bool) -> Shot {
    let errors = file.with_extension("stderr");
    let result = (|| {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&errors)?);
        picker.run(command)
    })();
    match result {
        Ok(status) if status.success() => read(file),
        Ok(status) if interactive && status.code() == Some(1) && cancelled(&errors, file) => {
            Shot::Cancelled
        }
        Ok(status) => Shot::Failed(format!("screenshot tool ended with {status}")),
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => Shot::Cancelled,
        Err(_) => Shot::Failed("couldn't run the screenshot tool".into()),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn cancelled(errors: &Path, output: &Path) -> bool {
    let empty = match std::fs::symlink_metadata(output) {
        Ok(metadata) => metadata.is_file() && metadata.len() == 0,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    };
    empty
        && bounded(errors, 1024)
            .is_ok_and(|bytes| bytes.is_empty() || bytes == b"screencapture: User canceled\n")
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

/// `slurp` prints the picked region; a cancel ends it unsuccessfully.
#[cfg(target_os = "linux")]
fn grim(file: &Path, picker: &Picker) -> Shot {
    let region_file = file.with_extension("region");
    let errors = file.with_extension("stderr");
    let result = (|| {
        picker.run(
            Command::new("slurp")
                .stdin(Stdio::null())
                .stdout(std::fs::File::create(&region_file)?)
                .stderr(std::fs::File::create(&errors)?),
        )
    })();
    match result {
        Ok(status) if status.success() => {}
        Ok(status) if status.code() == Some(1) && cancelled(&errors, &region_file) => {
            return Shot::Cancelled;
        }
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => return Shot::Cancelled,
        _ => return Shot::Failed("the region picker failed".into()),
    }
    let Ok(region) = bounded(&region_file, 1024)
        .and_then(|bytes| String::from_utf8(bytes).map_err(std::io::Error::other))
    else {
        return Shot::Failed("the region picker returned an invalid region".into());
    };
    if region.trim().is_empty() {
        return Shot::Failed("the region picker returned an empty region".into());
    }
    picked(
        Command::new("grim").arg("-g").arg(region.trim()).arg(file),
        file,
        picker,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[0, 0, 0, 255])
                .unwrap();
        }
        bytes
    }

    #[test]
    fn invalid_and_truncated_pngs_are_not_shots() {
        let dir = scratch().unwrap();
        let file = dir.join("shot.png");
        let valid = png();
        for bytes in [
            b"not a PNG".as_slice(),
            b"\x89PNG\r\n\x1a\n",
            &valid[..valid.len() - 12],
        ] {
            std::fs::write(&file, bytes).unwrap();
            assert!(matches!(read(&file), Shot::Failed(_)));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn an_inert_failed_picker_never_uploads_its_partial_file() {
        let dir = scratch().unwrap();
        let file = dir.join("shot.png");
        let command = |script: &str| {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script, "fake-picker"]).arg(&file);
            command
        };
        let picker = Picker::default();
        assert!(matches!(
            picked(
                &mut command("printf partial > \"$1\"; printf failure >&2; exit 1"),
                &file,
                &picker,
                true
            ),
            Shot::Failed(_)
        ));
        assert!(
            matches!(
                picked(&mut command("exit 1"), &file, &picker, true),
                Shot::Failed(_),
            ),
            "failed partial output is not a cancellation even without diagnostics"
        );
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            picked(&mut command("exit 1"), &file, &picker, true),
            Shot::Cancelled,
        );
        assert!(matches!(
            picked(&mut command("exit 2"), &file, &picker, true),
            Shot::Failed(_)
        ));
        assert!(matches!(
            picked(&mut command("exit 1"), &file, &picker, false),
            Shot::Failed(_)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bounded_read_accepts_exact_limit_and_rejects_limit_plus_one() {
        let dir = scratch().unwrap();
        let file = dir.join("bytes");
        std::fs::write(&file, b"1234").unwrap();
        assert_eq!(bounded(&file, 4).unwrap(), b"1234");
        assert!(bounded(&file, 3).is_err());
        assert!(bounded(&dir, 10).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn png_dimensions_and_crc_are_validated_without_trusting_the_extension() {
        let mut oversized = Vec::new();
        {
            let encoder = png::Encoder::new(&mut oversized, 8193, 1);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&vec![0; 8193])
                .unwrap();
        }
        assert!(validate_png(&oversized).is_err());
        let mut corrupt = png();
        let end = corrupt.len() - 1;
        corrupt[end] ^= 1;
        assert!(validate_png(&corrupt).is_err());
        assert_eq!(validate_png(&png()), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn screenshot_reads_refuse_symlinks_and_cleanup_owns_its_folder() {
        let dir = scratch().unwrap();
        let target = dir.join("real.png");
        let link = dir.join("shot.png");
        std::fs::write(&target, png()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(read(&link), Shot::Failed(_)));
        let cleanup = Cleanup(dir.clone());
        drop(cleanup);
        assert!(!dir.exists());
    }

    #[test]
    fn an_oversized_screenshot_is_refused_before_upload() {
        let dir = scratch().unwrap();
        let file = dir.join("shot.png");
        std::fs::File::create(&file)
            .unwrap()
            .set_len(phux_protocol::MAX_FILE_UPLOAD_SIZE + 1)
            .unwrap();
        assert!(matches!(read(&file), Shot::Failed(_)));
        std::fs::remove_dir_all(dir).unwrap();
    }

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
        let png = png();
        std::fs::write(&file, &png).unwrap();
        assert_eq!(read(&file), Shot::Taken(png));
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
