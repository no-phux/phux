//! Process entry: options, installed-CLI and server resolution, logging,
//! then GPUI + Kit initialization and the first window.
//!
//! Resolution never writes over or upgrades anything. An explicit `--phux`,
//! `--socket` or `--session` always wins over discovery, so a qualification
//! run can pin a same-checkout CLI and a temporary socket.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context as _, bail};
use phux_protocol::ResourceId;

use crate::app::{self, AppOptions};
use crate::diag;

/// Everything the command line can say.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Options {
    pub socket: Option<PathBuf>,
    pub session: Option<String>,
    pub phux: Option<PathBuf>,
    pub layout: Option<PathBuf>,
    pub fresh: bool,
    pub terminal: Option<String>,
    pub quick: bool,
    /// Directory for presentation evidence and `perf.jsonl`.
    pub evidence: Option<PathBuf>,
    /// Exit 0 once evidence of a presented frame is written.
    pub smoke: bool,
}

const USAGE: &str = "usage: phux-desktop [--socket PATH] [--session NAME] [--phux PATH] \
[--layout PATH] [--fresh] [--terminal ID] [--quick] [--evidence DIR] [--smoke]";

impl Options {
    /// Parse `args` (without the program name).
    pub fn parse(args: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value = |name: &str| {
                args.next()
                    .filter(|value| !value.is_empty())
                    .with_context(|| format!("{name} needs a value\n{USAGE}"))
            };
            match arg.as_str() {
                "--socket" => options.socket = Some(value("--socket")?.into()),
                "--session" => options.session = Some(value("--session")?),
                "--phux" => options.phux = Some(value("--phux")?.into()),
                "--layout" => options.layout = Some(value("--layout")?.into()),
                "--terminal" => options.terminal = Some(value("--terminal")?),
                "--evidence" => options.evidence = Some(value("--evidence")?.into()),
                "--fresh" => options.fresh = true,
                "--quick" => options.quick = true,
                "--smoke" => options.smoke = true,
                // macOS passes a process serial number to apps opened by Finder.
                arg if arg.starts_with("-psn_") => {}
                "-h" | "--help" => bail!("{USAGE}"),
                other => bail!("unknown argument {other:?}\n{USAGE}"),
            }
        }
        if options.smoke && options.evidence.is_none() {
            bail!("--smoke needs --evidence DIR\n{USAGE}");
        }
        Ok(options)
    }
}

/// Where installs land; a Finder-launched app has launchd's minimal `PATH`.
#[must_use]
pub fn installed_phux_candidates(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/bin/phux"),
        PathBuf::from("/opt/homebrew/bin/phux"),
        PathBuf::from("/usr/local/bin/phux"),
        home.join(".cargo/bin/phux"),
    ]
}

fn find_phux(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = explicit.or_else(|| std::env::var_os("PHUX_BIN").map(PathBuf::from)) {
        return Some(path);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if let Some(found) = installed_phux_candidates(&home).into_iter().find(|path| path.is_file()) {
        return Some(found);
    }
    let which = Command::new("/usr/bin/which").arg("phux").output().ok()?;
    let path = String::from_utf8(which.stdout).ok()?.trim().to_owned();
    (which.status.success() && !path.is_empty()).then(|| PathBuf::from(path))
}

fn phux_json(phux: &Path, args: &[&str]) -> anyhow::Result<serde_json::Value> {
    let output = Command::new(phux)
        .args(args)
        .output()
        .with_context(|| format!("could not run {}", phux.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`phux {}` failed: {}", args.join(" "), stderr.trim());
    }
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("`phux {}` printed no JSON", args.join(" ")))
}

fn ensure_server(phux: &Path, socket: Option<&Path>) -> anyhow::Result<PathBuf> {
    let mut ensure = Command::new(phux);
    if let Some(socket) = socket {
        ensure.arg("--socket").arg(socket);
    }
    let output = ensure
        .args(["server", "--ensure"])
        .output()
        .with_context(|| format!("could not run {}", phux.display()))?;
    if !output.status.success() {
        bail!("`phux server --ensure` failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    if let Some(socket) = socket {
        return Ok(socket.to_owned());
    }
    let status = phux_json(phux, &["status", "--json"])?;
    status["socket"]
        .as_str()
        .map(PathBuf::from)
        .context("no running phux server after `phux server --ensure`")
}

fn session_names(phux: &Path, socket: &Path) -> Vec<String> {
    let socket = socket.to_string_lossy();
    phux_json(phux, &["--socket", &socket, "status", "--json"])
        .ok()
        .and_then(|status| status["sessions"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|session| session["name"].as_str().map(str::to_owned))
        .collect()
}

/// An explicit choice, else `default`, else the first session, else `default`.
fn pick_session(requested: Option<String>, names: &[String]) -> String {
    requested
        .or_else(|| names.iter().any(|name| name == "default").then(|| "default".to_owned()))
        .or_else(|| names.first().cloned())
        .unwrap_or_else(|| "default".to_owned())
}

fn ensure_session(phux: &Path, socket: &Path, name: &str, names: &[String]) -> anyhow::Result<()> {
    if names.iter().any(|known| known == name) {
        return Ok(());
    }
    let socket = socket.to_string_lossy();
    phux_json(phux, &["--socket", &socket, "new", "--json", "-s", name])
        .map(drop)
        .with_context(|| format!("could not create phux session {name:?}"))
}

/// What the window opens on. A failure still opens the window, with the
/// reason shown in place of the inventory.
struct Resolved {
    socket: PathBuf,
    session: String,
    phux: PathBuf,
    error: Option<String>,
}

fn resolve(options: &Options) -> Resolved {
    let explicit_socket = options
        .socket
        .clone()
        .or_else(|| std::env::var_os("PHUX_SOCKET").map(PathBuf::from));
    let requested = options
        .session
        .clone()
        .or_else(|| std::env::var("PHUX_SESSION").ok().filter(|name| !name.is_empty()));
    let Some(phux) = find_phux(options.phux.clone()) else {
        return Resolved {
            socket: explicit_socket.unwrap_or_default(),
            session: requested.unwrap_or_else(|| "default".to_owned()),
            phux: PathBuf::from("phux"),
            error: Some(
                "Install the phux CLI from https://phux.sh/install so the desktop can start a server."
                    .to_owned(),
            ),
        };
    };
    let socket = match ensure_server(&phux, explicit_socket.as_deref()) {
        Ok(socket) => socket,
        Err(error) => {
            return Resolved {
                socket: explicit_socket.unwrap_or_default(),
                session: requested.unwrap_or_else(|| "default".to_owned()),
                phux,
                error: Some(format!("{error:#}")),
            };
        }
    };
    let names = session_names(&phux, &socket);
    let session = pick_session(requested, &names);
    let error = ensure_session(&phux, &socket, &session, &names)
        .err()
        .map(|error| format!("{error:#}"));
    Resolved { socket, session, phux, error }
}

/// Run the desktop until its last window closes.
#[must_use]
pub fn main() -> ExitCode {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error:#}");
            return ExitCode::from(2);
        }
    };
    diag::init_logging();
    let resolved = resolve(&options);
    log::info!(
        "launch socket={} session={} phux={} error={:?}",
        resolved.socket.display(),
        resolved.session,
        resolved.phux.display(),
        resolved.error
    );
    let initial_terminal = match options.terminal.as_deref().map(parse_terminal).transpose() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("{error:#}");
            return ExitCode::from(2);
        }
    };
    let app_options = AppOptions {
        socket: resolved.socket,
        session: resolved.session,
        phux: resolved.phux,
        layout_path: options.layout.clone().unwrap_or_default(),
        fresh: options.fresh,
        initial_terminal,
        quick: options.quick,
        startup_error: resolved.error,
    };
    let evidence = options.evidence.clone().map(|dir| diag::EvidenceOptions {
        dir,
        exit_after: options.smoke,
    });
    gpui_platform::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            diag::install(evidence, cx);
            if let Err(error) = app::open(app_options, cx) {
                log::error!("could not open the desktop window: {error:#}");
                eprintln!("could not open the desktop window: {error:#}");
                cx.quit();
            }
        });
    diag::exit_code()
}

fn parse_terminal(text: &str) -> anyhow::Result<ResourceId> {
    phux_client_ffi::projection::id::parse(text)
        .with_context(|| format!("--terminal {text:?} is not a terminal id (local:N or satellite:HOST:N)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Options> {
        Options::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn every_documented_flag_parses() {
        let options = parse(&[
            "--socket", "/tmp/s", "--session", "w", "--phux", "/bin/phux", "--layout", "/tmp/l.json",
            "--fresh", "--terminal", "local:3", "--quick", "--evidence", "/tmp/e", "--smoke",
        ])
        .expect("valid");
        assert_eq!(options.socket.as_deref(), Some(Path::new("/tmp/s")));
        assert_eq!(options.session.as_deref(), Some("w"));
        assert!(options.fresh && options.quick && options.smoke);
        assert_eq!(options.terminal.as_deref(), Some("local:3"));
    }

    #[test]
    fn smoke_without_evidence_and_unknown_flags_are_refused() {
        assert!(parse(&["--smoke"]).is_err());
        assert!(parse(&["--sockets", "/tmp/s"]).is_err());
        assert!(parse(&["--socket"]).is_err());
        assert!(parse(&["--socket", ""]).is_err());
        assert_eq!(parse(&["-psn_0_12345"]).expect("finder arg"), Options::default());
    }

    #[test]
    fn session_choice_prefers_explicit_then_default_then_first() {
        let names = ["work".to_owned(), "default".to_owned()];
        assert_eq!(pick_session(Some("x".into()), &names), "x");
        assert_eq!(pick_session(None, &names), "default");
        assert_eq!(pick_session(None, &names[..1]), "work");
        assert_eq!(pick_session(None, &[]), "default");
    }
}
