//! The native app's CLI lives beside its coordinator: in WSL on Windows,
//! on this OS elsewhere. Arguments remain separate, never shell source.

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
static CLI: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
#[cfg(windows)]
static WSL_HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

pub(crate) fn configure_cli(phux: &Path) {
    #[cfg(windows)]
    let _ = CLI.set(phux.to_path_buf());
    #[cfg(not(windows))]
    let _ = phux;
}

/// Resolve once before entering GPUI; a menu command must not block on WSL.
pub(crate) fn prepare_coordinator_home() {
    #[cfg(windows)]
    WSL_HOME.get_or_init(|| {
        let output = wsl_command()
            .args(["--exec", "sh", "-lc", "printf '%s' \"$HOME\""])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| parse_coordinator_home(&output.stdout))
            .flatten()
    });
}

pub(crate) fn coordinator_home() -> Option<String> {
    #[cfg(windows)]
    {
        WSL_HOME.get().cloned().flatten()
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").ok().filter(|home| !home.is_empty())
    }
}

#[cfg(any(windows, test))]
fn parse_coordinator_home(bytes: &[u8]) -> Option<String> {
    let home = std::str::from_utf8(bytes).ok()?;
    (home.starts_with('/') && !home.contains(['\0', '\n', '\r'])).then(|| home.to_owned())
}

pub(crate) fn local_target(socket: PathBuf) -> phux_client_runtime::Target {
    #[cfg(windows)]
    {
        phux_client_runtime::Target::wsl(
            socket,
            CLI.get().cloned().unwrap_or_else(|| "phux".into()),
        )
    }
    #[cfg(not(windows))]
    {
        phux_client_runtime::Target::uds(socket)
    }
}

/// A phux CLI command on the coordinator's platform.
pub(crate) fn phux_command(phux: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut command = wsl_command();
        command
            .args([
                "--exec",
                "sh",
                "-lc",
                "export PATH=\"$HOME/.local/bin:$PATH\"; exec \"$@\"",
                "phux-desktop",
            ])
            .arg(phux);
        command
    }
    #[cfg(not(windows))]
    Command::new(phux)
}

pub(crate) fn validate_coordinator() -> Result<(), String> {
    #[cfg(windows)]
    {
        let output = wsl_command()
            .args(["--exec", "id", "-u"])
            .output()
            .map_err(|error| {
                format!(
                    "Could not start WSL: {error}. Install WSL 2 and initialize a Linux user first."
                )
            })?;
        if !output.status.success() {
            return Err("Could not start the selected WSL distribution. Run wsl --update, initialize its non-root Linux user, then install phux inside it.".into());
        }
        let uid = std::str::from_utf8(&output.stdout)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok());
        if !uid.is_some_and(|uid| uid != 0) {
            return Err("Phux Desktop needs a non-root default WSL user. Initialize Ubuntu and select that user before opening the app.".into());
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn wsl_command() -> Command {
    use std::os::windows::process::CommandExt as _;
    let mut command = Command::new("wsl.exe");
    if let Some(distro) = std::env::var_os("PHUX_WSL_DISTRO").filter(|name| !name.is_empty()) {
        command.arg("--distribution").arg(distro);
    }
    // CLI helpers are pipes, not interactive console windows.
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    command
}

/// A folder selected with the native picker, expressed on the coordinator.
pub(crate) fn coordinator_path(path: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        if path.to_str().is_some_and(|path| path.starts_with('/')) {
            return Ok(path.to_path_buf());
        }
        let output = wsl_command()
            .args(["--exec", "wslpath", "-a", "-u"])
            .arg(path)
            .output()
            .map_err(|error| format!("Could not translate the Windows folder to WSL: {error}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let text = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
        let translated = text.trim();
        if !translated.starts_with('/') {
            return Err("WSL returned no absolute folder path.".into());
        }
        Ok(PathBuf::from(translated))
    }
    #[cfg(not(windows))]
    Ok(path.to_path_buf())
}

pub(crate) fn coordinator_absolute(path: &str) -> bool {
    #[cfg(windows)]
    {
        path.starts_with('/')
    }
    #[cfg(not(windows))]
    {
        Path::new(path).is_absolute()
    }
}

/// User-owned native state. Never put Windows preferences in the WSL home.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

pub(crate) fn state_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("phux-desktop")
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().unwrap_or_default().join(".local/state"))
            .join("phux-desktop")
    }
}

pub(crate) fn local_machine_label() -> &'static str {
    if cfg!(windows) { "WSL" } else { "This Mac" }
}

/// WSL owns a different filesystem even when its server is this app's home.
pub(crate) fn filesystem_host(remote: Option<&str>) -> Option<&str> {
    if cfg!(windows) {
        remote.or(Some("WSL"))
    } else {
        remote
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn coordinator_paths_are_not_native_windows_paths() {
        assert!(super::coordinator_absolute("/home/user/project"));
        assert!(!super::coordinator_absolute("relative/project"));
        #[cfg(windows)]
        assert!(!super::coordinator_absolute(r"C:\Users\user"));
    }

    #[test]
    fn coordinator_home_is_a_linux_path_not_a_windows_environment_value() {
        assert_eq!(
            super::parse_coordinator_home(b"/home/user"),
            Some("/home/user".into())
        );
        for invalid in [
            b"".as_slice(),
            br"C:\Users\user",
            b"relative",
            b"/home/user\nnoise",
        ] {
            assert_eq!(super::parse_coordinator_home(invalid), None);
        }
    }

    #[cfg(windows)]
    #[test]
    fn wsl_attachments_use_uploads_not_windows_paths() {
        let host = crate::attach::terminal_host(
            super::filesystem_host(None),
            &phux_protocol::ResourceId::local(1),
        );
        let result = crate::attach::attach_paths(
            &host,
            &[std::path::PathBuf::from(r"C:\Users\person\note.txt")],
        );
        assert!(matches!(result, Some(crate::attach::PathAttach::Upload(_))));
    }

    #[cfg(windows)]
    #[test]
    fn wsl_cli_arguments_stay_data() {
        let mut command = super::phux_command(std::path::Path::new("/home/λ/bin/phux"));
        command.args([
            "--socket",
            "/run/a 'quoted.sock",
            "new",
            "--json",
            "-s",
            "$(touch sentinel)",
        ]);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.ends_with(&[
            "--socket".into(),
            "/run/a 'quoted.sock".into(),
            "new".into(),
            "--json".into(),
            "-s".into(),
            "$(touch sentinel)".into()
        ]));
        assert!(args.iter().any(|arg| arg == "/home/λ/bin/phux"));
    }
}
