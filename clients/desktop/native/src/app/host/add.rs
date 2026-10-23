//! `phux host add` with its progress read line by line.

use std::path::Path;
use std::process::Command;

use super::{MISSING_CLI, args};

/// What `phux host add` is asked to do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddHost {
    pub target: String,
    pub name: String,
    /// `--install`: install phux there when missing, update it when older.
    pub install: bool,
    /// `--accept-new-host-key`: the tailnet vouches for the address.
    pub accept_new_host_key: bool,
}

impl AddHost {
    fn args(&self) -> Vec<String> {
        let mut args = args(&["host", "add", self.target.trim()]);
        if !self.name.trim().is_empty() {
            args.extend(["--name".to_owned(), self.name.trim().to_owned()]);
        }
        if self.install {
            args.push("--install".to_owned());
        }
        if self.accept_new_host_key {
            args.push("--accept-new-host-key".to_owned());
        }
        args
    }
}

/// `phux host add TARGET` over the ssh trust the user already has.
/// Enrollment can install phux, installs the remote service and pairs, so it
/// takes a while: each progress line it prints goes to `progress` as it
/// arrives.
pub fn add_host(
    phux: &Path,
    request: &AddHost,
    progress: impl FnMut(String),
) -> Result<Vec<String>, String> {
    if request.target.trim().is_empty() {
        return Err("Enter an ssh destination such as me@mini.".into());
    }
    if phux.as_os_str().is_empty() {
        return Err(MISSING_CLI.into());
    }
    // Plain diagnostics: the progress lines are read by their prefix.
    let mut child = Command::new(phux)
        .args(request.args())
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not run {}: {error}", phux.display()))?;
    let stderr = child.stderr.take().map(read_progress(progress));
    let status = child
        .wait()
        .map_err(|error| format!("Could not run {}: {error}", phux.display()))?;
    let lines = stderr.unwrap_or_default();
    if status.success() {
        return Ok(lines
            .iter()
            .filter(|line| line.starts_with("phux: warn: "))
            .filter_map(|line| progress_step(line))
            .collect());
    }
    let failure =
        failure_text(&lines).unwrap_or_else(|| format!("{} exited {status}", phux.display()));
    if failure.contains("unexpected argument '--install'")
        || failure.contains("unexpected argument '--accept-new-host-key'")
    {
        return Err("Installing phux on the machine needs a newer phux CLI here. Update it from Settings > Updates, or turn off installing.".into());
    }
    Err(failure)
}

/// Read the CLI's diagnostics line by line, reporting each step as it comes;
/// every line is kept for the failure text.
fn read_progress(
    mut progress: impl FnMut(String),
) -> impl FnOnce(std::process::ChildStderr) -> Vec<String> {
    move |stderr| {
        use std::io::BufRead as _;
        let mut lines = Vec::new();
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(step) = progress_step(&line) {
                progress(step);
            }
            lines.push(line);
        }
        lines
    }
}

/// `phux: info: mini: trying quic://...` reads `trying quic://...`.
fn progress_step(line: &str) -> Option<String> {
    let rest = line.strip_prefix("phux: ")?;
    let (severity, text) = rest.split_once(": ")?;
    if !matches!(severity, "info" | "ok" | "warn") {
        return None;
    }
    let text = text
        .split_once(": ")
        .filter(|(name, _)| !name.contains(' '))
        .map_or(text, |(_, step)| step);
    Some(text.trim().to_owned()).filter(|step| !step.is_empty())
}

/// The error and its hint, without the progress that came before them.
fn failure_text(lines: &[String]) -> Option<String> {
    let start = lines
        .iter()
        .position(|line| line.starts_with("phux: error") || line.starts_with("error"))
        .unwrap_or(0);
    let text = lines[start..]
        .iter()
        .map(|line| {
            let line = line.trim();
            line.strip_prefix("phux: error: ")
                .or_else(|| line.strip_prefix("hint: "))
                .unwrap_or(line)
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::{AddHost, failure_text, progress_step};

    #[test]
    fn add_host_passes_only_the_flags_asked_for() {
        let plain = AddHost {
            target: " me@mini ".into(),
            ..AddHost::default()
        };
        assert_eq!(plain.args(), ["host", "add", "me@mini"]);
        let tailnet = AddHost {
            target: "mini.tail9a8b.ts.net".into(),
            name: "mini".into(),
            install: true,
            accept_new_host_key: true,
        };
        assert_eq!(
            tailnet.args(),
            [
                "host",
                "add",
                "mini.tail9a8b.ts.net",
                "--name",
                "mini",
                "--install",
                "--accept-new-host-key"
            ]
        );
    }

    #[test]
    fn enrollment_progress_and_failures_read_as_steps() {
        assert_eq!(
            progress_step("phux: info: mini: trying quic://100.70.0.2:8788").as_deref(),
            Some("trying quic://100.70.0.2:8788")
        );
        assert_eq!(
            progress_step("phux: ok: mini: paired").as_deref(),
            Some("paired")
        );
        assert_eq!(progress_step("phux: error: no"), None);
        assert_eq!(progress_step("Registered mini -> quic://x"), None);
        let lines = [
            "phux: info: mini: trying quic://x",
            "phux: error: cannot reach mini over ssh: refused",
            "  hint: check that `ssh mini` works",
        ]
        .map(str::to_owned);
        assert_eq!(
            failure_text(&lines).as_deref(),
            Some("cannot reach mini over ssh: refused\ncheck that `ssh mini` works")
        );
        assert_eq!(failure_text(&[]), None);
    }
}
