//! `phux host add --install`: put phux on the far host, or bring an older
//! one up to this machine's release, before enrolling it.
//!
//! Only with the operator's explicit `--install`: the official installer
//! (`https://phux.sh/install`) runs over the same ssh trust, on this
//! machine's release channel, into the installer's default `~/.local/bin`.
//! An older phux is updated with its own `phux update`, which keeps the live
//! server's panes through the handoff and refuses package-managed installs;
//! a refused or failed update is a warning, never a failed enrollment.

use phux_client_runtime::release::{Channel, Version};

use super::events::{EnrollEvent, EnrollFailure};
use super::server::remote_phux_version;
use super::ssh::ssh_run;

/// Where the official installer is served.
const INSTALLER_URL: &str = "https://phux.sh/install";

/// The `phux` to enroll with: `remote_phux` when it answers (updated first
/// when it is older than this one), else the one the installer just put in
/// place.
///
/// # Errors
///
/// ssh failing, or phux still missing after the installer ran.
pub(crate) fn provision_remote_phux(
    ssh_host: &str,
    remote_phux: &str,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Result<String, EnrollFailure> {
    match remote_phux_version(ssh_host, remote_phux) {
        Ok(version) => {
            update_if_older(ssh_host, remote_phux, version.trim(), on_event);
            Ok(remote_phux.to_owned())
        }
        Err(EnrollFailure::MissingPhux(_)) => match installed_before(ssh_host) {
            // An earlier `--install` put it where a non-interactive shell
            // does not look: use (and update) that one, not a reinstall.
            Some((phux, version)) => {
                update_if_older(ssh_host, &phux, version.trim(), on_event);
                Ok(phux)
            }
            None => install(ssh_host, on_event),
        },
        Err(other) => Err(other),
    }
}

/// The phux the installer put in its default directory, when it runs: its
/// word for later commands and its `--version` line.
fn installed_before(ssh_host: &str) -> Option<(String, String)> {
    let probe = format!("{INSTALL_DIR}\nprintf '%s\\n' \"$dir/phux\"\n");
    let out = ssh_run(ssh_host, &["sh", "-c", &shell_quote(&probe)]).ok()?;
    let path = out
        .stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('/'))?;
    let phux = shell_word(path);
    let version = remote_phux_version(ssh_host, &phux).ok()?;
    Some((phux, version))
}

/// Where the installer puts phux, as an absolute `$dir`.
const INSTALL_DIR: &str = "dir=\"${PHUX_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}\"\n\
                           case \"$dir\" in /*) ;; *) dir=\"$PWD/$dir\" ;; esac";

fn install(ssh_host: &str, on_event: &mut dyn FnMut(EnrollEvent)) -> Result<String, EnrollFailure> {
    let channel = super::super::update::channel::of_build();
    on_event(EnrollEvent::PhuxInstalling {
        channel: channel.as_str().to_owned(),
    });
    let script = install_script(channel);
    let out = ssh_run(ssh_host, &["sh", "-c", &shell_quote(&script)])
        .map_err(EnrollFailure::SshUnreachable)?;
    if !out.status.success() {
        return Err(EnrollFailure::InstallFailed(format!(
            "the phux installer failed: {}",
            last_lines(&out.failure_detail(), 6)
        )));
    }
    let installed = out
        .stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('/'))
        .map(shell_word)
        .ok_or_else(|| {
            EnrollFailure::InstallFailed(
                "the phux installer did not say where it put phux".to_owned(),
            )
        })?;
    let version = remote_phux_version(ssh_host, &installed).map_err(|failure| match failure {
        EnrollFailure::MissingPhux(detail) => {
            EnrollFailure::InstallFailed(format!("the installed phux does not run: {detail}"))
        }
        other => other,
    })?;
    on_event(EnrollEvent::PhuxInstalled {
        version: version.trim().to_owned(),
        path: installed.clone(),
    });
    Ok(installed)
}

/// Download the installer to a private temp file (a failed download must
/// not pipe nothing into `sh` and look like success), run it with its
/// output on stderr, and print the installed binary's absolute path last.
fn install_script(channel: Channel) -> String {
    // The channel is explicit: the installer otherwise follows the far
    // host's `$PHUX_CHANNEL`.
    let channel = channel.as_str();
    format!(
        "set -eu\n\
         {INSTALL_DIR}\n\
         tmp=$(mktemp)\n\
         trap 'rm -f \"$tmp\"' EXIT\n\
         if command -v curl >/dev/null 2>&1; then curl -fsSL {INSTALLER_URL} -o \"$tmp\"\n\
         elif command -v wget >/dev/null 2>&1; then wget -qO \"$tmp\" {INSTALLER_URL}\n\
         else echo 'neither curl nor wget is installed' >&2; exit 1; fi\n\
         sh \"$tmp\" --channel {channel} --install-dir \"$dir\" >&2\n\
         printf '%s\\n' \"$dir/phux\"\n"
    )
}

/// Update a far phux older than this one with its own `phux update`, on
/// this machine's channel.
fn update_if_older(
    ssh_host: &str,
    remote_phux: &str,
    remote_version: &str,
    on_event: &mut dyn FnMut(EnrollEvent),
) {
    let local = env!("PHUX_VERSION_LABEL");
    if !is_older(remote_version, local) {
        return;
    }
    on_event(EnrollEvent::PhuxUpdating {
        from: remote_version.to_owned(),
        to: local.to_owned(),
    });
    let mut argv = vec![remote_phux, "update"];
    if super::super::update::channel::of_build() == Channel::Next {
        argv.extend(["--channel", "next"]);
    }
    let result = ssh_run(ssh_host, &argv).and_then(|out| {
        if out.status.success() {
            Ok(())
        } else {
            Err(last_lines(&out.failure_detail(), 4))
        }
    });
    match result.and_then(|()| {
        remote_phux_version(ssh_host, remote_phux).map_err(|failure| failure.detail().to_owned())
    }) {
        Ok(version) => on_event(EnrollEvent::PhuxUpdated {
            version: version.trim().to_owned(),
        }),
        Err(reason) => on_event(EnrollEvent::PhuxNotUpdated { reason }),
    }
}

/// Whether the far `phux --version` line names an older release than
/// `local` (`phux 0.40.1`, `phux 0.41.0+next.abc1234`). An unreadable line
/// is never older: nothing is updated on a guess.
fn is_older(remote: &str, local: &str) -> bool {
    let core = |text: &str| {
        let token = text.split_whitespace().last()?;
        Version::parse(token.split('+').next()?)
    };
    matches!((core(remote), core(local)), (Some(remote), Some(local)) if remote < local)
}

fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

/// One POSIX-shell word: ssh joins its arguments into one remote command
/// line, so the script must reach `sh -c` intact.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// A path as one word of the far command line: bare when no shell treats a
/// character in it specially, quoted otherwise (a home with a space).
fn shell_word(path: &str) -> String {
    let plain = path
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/._-+@".contains(&byte));
    if plain {
        path.to_owned()
    } else {
        shell_quote(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_readable_lower_release_is_older() {
        assert!(is_older("phux 0.40.1", "phux 0.41.0"));
        assert!(is_older("phux 0.40.1", "0.41.0+next.abc1234"));
        assert!(is_older("phux 0.9.9+next.0123456", "0.10.0"));
        assert!(!is_older("phux 0.41.0", "0.41.0"));
        assert!(
            !is_older("phux 0.41.0+next.aaa", "0.41.0+next.bbb"),
            "same core"
        );
        assert!(!is_older("phux 0.42.0", "0.41.0"));
        assert!(!is_older("garbage", "0.41.0"));
        assert!(!is_older("", "0.41.0"));
    }

    #[test]
    fn the_installer_script_follows_the_channel_and_reports_an_absolute_path() {
        let stable = install_script(Channel::Stable);
        assert!(
            stable.contains("sh \"$tmp\" --channel stable --install-dir \"$dir\" >&2\n"),
            "{stable}"
        );
        assert!(stable.contains("curl -fsSL https://phux.sh/install -o \"$tmp\""));
        let next = install_script(Channel::Next);

        // The script itself runs: a stub installer stands in for the download.
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let curl = bin.join("curl");
        std::fs::write(
            &curl,
            "#!/bin/sh\nwhile [ \"$1\" != -o ]; do shift; done\nprintf 'echo installing \"$@\"\\n' > \"$2\"\n",
        )
        .expect("stub curl");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&next)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", dir.path())
            .env_remove("PHUX_INSTALL_DIR")
            .output()
            .expect("run script");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            format!("{}/.local/bin/phux", dir.path().display())
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(&format!(
                "installing --channel next --install-dir {}/.local/bin",
                dir.path().display()
            )),
            "{out:?}"
        );
    }

    #[test]
    fn a_failed_download_fails_the_script() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let curl = bin.join("curl");
        std::fs::write(&curl, "#!/bin/sh\necho 'could not resolve' >&2\nexit 6\n").expect("stub");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(install_script(Channel::Stable))
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", dir.path())
            .output()
            .expect("run script");
        assert!(!out.status.success());
        assert!(
            out.stdout.is_empty(),
            "no path reported for a failed install"
        );
    }

    #[test]
    fn an_installed_path_is_one_word_on_the_far_command_line() {
        assert_eq!(
            shell_word("/home/me/.local/bin/phux"),
            "/home/me/.local/bin/phux"
        );
        assert_eq!(
            shell_word("/Users/Pat Smith/.local/bin/phux"),
            "'/Users/Pat Smith/.local/bin/phux'"
        );
    }

    #[test]
    fn quoting_survives_the_remote_shell() {
        let script = "printf '%s\\n' \"it's\"";
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("sh -c {}", shell_quote(script)))
            .output()
            .expect("sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "it's\n");
    }
}
