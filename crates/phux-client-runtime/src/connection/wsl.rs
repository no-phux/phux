//! WSL is a child-stdio lane, not a network listener or a Windows Unix socket.

use std::path::Path;

#[cfg(windows)]
use super::io::Io;
use crate::dial::Dial;

#[cfg(windows)]
pub(super) fn plan(socket: &Path, phux: &Path) -> Result<Dial, String> {
    validate(socket, phux.as_os_str()).map_err(|error| error.to_string())?;
    Ok(Dial::Wsl {
        socket: socket.to_path_buf(),
        phux: phux.to_path_buf(),
    })
}

#[cfg(not(windows))]
pub(super) fn plan(_socket: &Path, _phux: &Path) -> Result<Dial, String> {
    Err("the WSL transport requires native Windows".to_owned())
}

#[cfg(windows)]
pub(super) fn establish(socket: &Path, phux: &Path) -> std::io::Result<Io> {
    let distro = std::env::var_os("PHUX_WSL_DISTRO").filter(|name| !name.is_empty());
    let mut command = command(socket, distro.as_deref(), phux.as_os_str())?;
    // The bridge is a pipe, not an interactive console window.
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    Io::child(&mut command)
}

#[cfg(any(windows, test))]
fn command(
    socket: &Path,
    distro: Option<&std::ffi::OsStr>,
    phux: &std::ffi::OsStr,
) -> std::io::Result<tokio::process::Command> {
    validate(socket, phux)?;
    let mut command = tokio::process::Command::new("wsl.exe");
    if let Some(distro) = distro {
        command.arg("--distribution").arg(distro);
    }
    command.args([
        "--exec",
        "sh",
        "-c",
        "export PATH=\"$HOME/.local/bin:$PATH\"; exec \"$@\"",
        "phux-desktop",
    ]);
    command
        .arg(phux)
        .arg("--socket")
        .arg(socket)
        .arg("stdio-bridge");
    Ok(command)
}

#[cfg(any(windows, test))]
fn validate(socket: &Path, phux: &std::ffi::OsStr) -> std::io::Result<()> {
    let path = socket.as_os_str().to_string_lossy();
    if !path.starts_with('/') || path.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a WSL server socket must be an absolute Linux path",
        ));
    }
    if phux.is_empty() || phux.to_string_lossy().contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the WSL phux executable must be nonempty and contain no NUL",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    #[test]
    fn distribution_binary_and_socket_are_arguments_not_shell_source() {
        let socket = Path::new("/run/user/1000/a 'socket; $(touch surprise).sock");
        let command = command(
            socket,
            Some(OsStr::new("Ubuntu Test")),
            OsStr::new("/a b/phux"),
        )
        .expect("valid Linux socket");
        let args: Vec<_> = command.as_std().get_args().map(OsString::from).collect();
        assert_eq!(command.as_std().get_program(), "wsl.exe");
        assert_eq!(
            args,
            [
                "--distribution",
                "Ubuntu Test",
                "--exec",
                "sh",
                "-c",
                "export PATH=\"$HOME/.local/bin:$PATH\"; exec \"$@\"",
                "phux-desktop",
                "/a b/phux",
                "--socket",
                socket.to_str().expect("utf8 fixture"),
                "stdio-bridge",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn default_distribution_omits_the_selector() {
        let command = command(Path::new("/a.sock"), None, OsStr::new("phux")).expect("command");
        assert_eq!(
            command.as_std().get_args().next(),
            Some(OsStr::new("--exec"))
        );
    }

    #[test]
    fn windows_and_relative_socket_paths_are_refused() {
        for path in ["", "relative.sock", "C:\\socket", "/bad\0socket"] {
            let error = command(Path::new(path), None, OsStr::new("phux")).expect_err("invalid");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn a_missing_explicit_binary_never_falls_back_to_an_installed_cli() {
        for phux in ["", "phux\0other"] {
            let error = command(Path::new("/a.sock"), None, OsStr::new(phux))
                .expect_err("invalid explicit executable");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_shell_wrapper_executes_the_exact_binary_and_preserves_socket_bytes() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(".profile"),
            "echo LOGIN_BANNER_MUST_NOT_REACH_PROTOCOL\n",
        )
        .expect("noisy login profile");
        let phux = dir.path().join("custom phux 'binary");
        std::fs::write(&phux, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").expect("fixture");
        std::fs::set_permissions(&phux, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        let socket = "/a 'socket; $(touch injected).sock";
        let wsl = command(Path::new(socket), None, phux.as_os_str()).expect("command");
        let args: Vec<_> = wsl.as_std().get_args().collect();
        // Run the exact argv after WSL's --exec using the host's POSIX shell.
        let output = tokio::process::Command::new(args[1])
            .args(&args[2..])
            .env("HOME", dir.path())
            .output()
            .await
            .expect("shell wrapper");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            output.stdout,
            format!("--socket\n{socket}\nstdio-bridge\n").as_bytes()
        );
    }

    #[tokio::test]
    async fn wsl_target_plans_as_local_stdio_not_a_remote_or_native_socket() {
        let target = crate::Target::wsl("/linux.sock".into(), "/custom/phux".into());
        #[cfg(windows)]
        assert!(
            matches!(target.plan().await.expect("plan"), crate::dial::Dial::Wsl { socket, phux }
            if socket == Path::new("/linux.sock") && phux == Path::new("/custom/phux"))
        );
        #[cfg(not(windows))]
        assert!(
            target
                .plan()
                .await
                .expect_err("not Windows")
                .contains("requires native Windows")
        );
        assert!(target.resolved().is_none());
        assert_eq!(target.transport.label(), "wsl");
    }

    #[cfg(not(windows))]
    #[test]
    fn other_platforms_do_not_silently_map_wsl_to_unix() {
        assert!(
            plan(Path::new("/linux.sock"), Path::new("phux"))
                .expect_err("unsupported")
                .contains("requires native Windows")
        );
    }
}
