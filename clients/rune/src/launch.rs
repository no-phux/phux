//! Command line, the installed CLI, and the server socket.
//!
//! The client never installs, replaces, or upgrades the CLI. It starts or
//! reuses the server through `phux server --ensure`, then asks the CLI which
//! socket that server listens on, the same way the GPUI desktop does.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const USAGE: &str = "usage: phux-rune [--socket PATH] [--session NAME] [--phux PATH]";

#[derive(Debug, Default)]
pub struct Options {
    pub socket: Option<PathBuf>,
    pub session: Option<String>,
    pub phux: Option<PathBuf>,
}

impl Options {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value = |name: &str| {
                args.next()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
            };
            match arg.as_str() {
                "--socket" => options.socket = Some(value("--socket")?.into()),
                "--session" => options.session = Some(value("--session")?),
                "--phux" => options.phux = Some(value("--phux")?.into()),
                // macOS passes a process serial number to apps Finder opens.
                arg if arg.starts_with("-psn_") => {}
                "-h" | "--help" => return Err(USAGE.to_owned()),
                other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
            }
        }
        Ok(options)
    }
}

/// Where the window connects, or why it can't.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub socket: Result<PathBuf, String>,
    pub session: Option<String>,
}

pub fn resolve(options: &Options) -> Resolved {
    let session = options
        .session
        .clone()
        .or_else(|| std::env::var("PHUX_SESSION").ok().filter(|s| !s.is_empty()));
    let explicit = options
        .socket
        .clone()
        .or_else(|| std::env::var_os("PHUX_SOCKET").map(PathBuf::from));
    if let Some(socket) = explicit {
        return Resolved {
            socket: Ok(socket),
            session,
        };
    }
    let socket = locate(options.phux.as_deref()).and_then(|phux| ensure_server(&phux));
    Resolved { socket, session }
}

/// `--phux`, else `PHUX_BIN`, else the usual install locations, else `PATH`.
/// Finder launches with a minimal `PATH`, so the fixed locations come first.
fn locate(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("PHUX_BIN").map(PathBuf::from))
    {
        return Ok(path);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let fixed = [
        home.as_ref().map(|h| h.join(".local/bin/phux")),
        Some(PathBuf::from("/opt/homebrew/bin/phux")),
        Some(PathBuf::from("/usr/local/bin/phux")),
        home.as_ref().map(|h| h.join(".cargo/bin/phux")),
    ];
    if let Some(found) = fixed.into_iter().flatten().find(|p| p.is_file()) {
        return Ok(found);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("phux"))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            "phux is not installed. Install it with `curl -fsSL https://phux.sh/install | sh`, \
             or pass --phux PATH."
                .to_owned()
        })
}

fn run(phux: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new(phux)
        .args(args)
        .output()
        .map_err(|error| format!("could not run {}: {error}", phux.display()))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "`phux {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn ensure_server(phux: &Path) -> Result<PathBuf, String> {
    run(phux, &["server", "--ensure"])?;
    let status: serde_json::Value = serde_json::from_slice(&run(phux, &["status", "--json"])?)
        .map_err(|error| format!("`phux status --json` printed no JSON: {error}"))?;
    status["socket"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "no running phux server after `phux server --ensure`".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn parses_every_flag() {
        let options = parse(&[
            "--socket",
            "/tmp/s",
            "--session",
            "work",
            "--phux",
            "/bin/phux",
        ])
        .expect("valid flags");
        assert_eq!(options.socket, Some(PathBuf::from("/tmp/s")));
        assert_eq!(options.session.as_deref(), Some("work"));
        assert_eq!(options.phux, Some(PathBuf::from("/bin/phux")));
    }

    #[test]
    fn ignores_the_finder_serial_number() {
        assert!(parse(&["-psn_0_12345"]).is_ok());
    }

    #[test]
    fn refuses_unknown_and_empty_values() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--socket"]).is_err());
        assert!(parse(&["--socket", ""]).is_err());
    }
}
