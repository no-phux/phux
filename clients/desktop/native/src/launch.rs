//! Process entry: options, installed-CLI and server resolution, logging,
//! then GPUI + Kit initialization and the first window.
//!
//! Resolution never writes over or upgrades anything. An explicit `--phux`,
//! `--socket` or `--session` always wins over discovery, so a qualification
//! run can pin a same-checkout CLI and a temporary socket. Without
//! `--session`, the window opens the owner's Intake, else the most recent
//! session; it never creates one ([`crate::connection::home`]).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use phux_protocol::ResourceId;

use crate::app::{self, AppOptions};
use crate::connection::home::open_home;
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
    /// Open the native performance monitor at launch; it can also be opened live.
    pub dev_telemetry: bool,
    /// Developer mode for this run ([`crate::devtools`]): heap accounting,
    /// the HUD and the dev socket, whatever the saved setting says.
    /// `PHUX_DEV=1` does the same.
    pub dev: bool,
    /// Directory for presentation evidence and `perf.jsonl`.
    pub evidence: Option<PathBuf>,
    /// Exit 0 once evidence of a presented frame is written.
    pub smoke: bool,
}

const USAGE: &str = "usage: phux-desktop [--socket PATH] [--session NAME] [--phux PATH] \
[--layout PATH] [--fresh] [--terminal ID] [--quick] [--dev] [--dev-telemetry] [--evidence DIR] [--smoke]";

impl Options {
    /// Explicit targets and qualification runs never consume an update's location.
    fn restores_update(&self) -> bool {
        self.session.is_none()
            && self.socket.is_none()
            && self.phux.is_none()
            && self.layout.is_none()
            && self.terminal.is_none()
            && self.evidence.is_none()
            && !self.fresh
            && !self.quick
            && !self.dev
            && ["PHUX_SESSION", "PHUX_SOCKET", "PHUX_BIN", "PHUX_PROFILE"]
                .iter()
                .all(|key| std::env::var_os(key).is_none())
    }

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
                "--dev-telemetry" => options.dev_telemetry = true,
                "--dev" => options.dev = true,
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

mod runtime;
pub(crate) mod shell_path;
pub(crate) use runtime::command_path;

/// `phux [--socket SOCKET] ARGS`'s stdout, or its stderr as the error.
fn run_phux(phux: &Path, socket: Option<&Path>, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let mut command = crate::platform::phux_command(phux);
    if let Some(socket) = socket {
        command.arg("--socket").arg(socket);
    }
    crate::updates::run_bounded(
        command.args(args),
        None,
        std::time::Duration::from_secs(30),
        "Starting phux timed out. Click Retry connection to try again; your saved sessions were not removed.",
    )
    .map(String::into_bytes)
    .with_context(|| format!("could not run {}", phux.display()))
}

pub(crate) fn ensure_server(phux: &Path, socket: Option<&Path>) -> anyhow::Result<PathBuf> {
    crate::platform::validate_coordinator().map_err(anyhow::Error::msg)?;
    run_phux(phux, socket, &["server", "--ensure"])?;
    crate::platform::prepare_coordinator_home();
    if let Some(socket) = socket {
        return Ok(socket.to_owned());
    }
    let status: serde_json::Value =
        serde_json::from_slice(&run_phux(phux, None, &["status", "--json"])?)
            .context("`phux status --json` printed no JSON")?;
    status["socket"]
        .as_str()
        .map(PathBuf::from)
        .context("no running phux server after `phux server --ensure`")
}

/// What the window opens on. A failure still opens the window, with the
/// reason shown in place of the inventory.
struct Resolved {
    socket: PathBuf,
    session: String,
    phux: PathBuf,
    error: Option<String>,
    /// The CLI discovery found or installed. Automatic discovery still runs
    /// later commands as `phux`, so this is the only record of the real path.
    cli: Option<crate::app::CliInstall>,
}

fn resolve(options: &Options) -> Resolved {
    let automatic = options.phux.is_none() && std::env::var_os("PHUX_BIN").is_none();
    match runtime::locate(options.phux.clone()) {
        Ok(located) => {
            let mut resolved = resolve_with(options, Some(located.path.clone()));
            note_discovered_cli(&mut resolved, located, automatic);
            resolved
        }
        Err(error) => {
            let mut resolved = resolve_with(options, None);
            resolved.error = Some(format!("{error:#}"));
            resolved
        }
    }
}

/// Record where discovery looked, and keep automatic launches on the `phux`
/// command so the next launch searches again instead of pinning a path.
fn note_discovered_cli(resolved: &mut Resolved, located: runtime::Located, automatic: bool) {
    resolved.cli = Some(crate::app::CliInstall {
        path: located.path,
        installed: located.installed,
    });
    if automatic {
        resolved.phux = "phux".into();
    }
}

/// [`resolve`] with the CLI already found, or `None` when none is installed.
fn resolve_with(options: &Options, phux: Option<PathBuf>) -> Resolved {
    if let Some(phux) = phux.as_deref() {
        crate::platform::configure_cli(phux);
    }
    let explicit_socket = options
        .socket
        .clone()
        .or_else(|| std::env::var_os("PHUX_SOCKET").map(PathBuf::from));
    #[cfg(windows)]
    if phux_config_core::instance::is_dev_build() && explicit_socket.is_none() {
        return Resolved {
            socket: PathBuf::new(),
            session: options.session.clone().unwrap_or_default(),
            phux: phux.unwrap_or_default(),
            error: Some("A development Windows build needs --socket pointing to an isolated WSL server. It will not start or connect to the installed server by default.".into()),
            cli: None,
        };
    }
    let requested = options.session.clone().or_else(|| {
        std::env::var("PHUX_SESSION")
            .ok()
            .filter(|name| !name.is_empty())
    });
    let (phux, socket, opened) = match phux {
        None => (
            PathBuf::from("phux"),
            explicit_socket.unwrap_or_default(),
            Err(runtime::MISSING.to_owned()),
        ),
        Some(phux) => match ensure_server(&phux, explicit_socket.as_deref()) {
            Ok(socket) => {
                let opened = open_home(&phux, &socket, requested.as_deref());
                (phux, socket, opened)
            }
            Err(error) => (
                phux,
                explicit_socket.unwrap_or_default(),
                Err(format!("{error:#}")),
            ),
        },
    };
    let (session, error) = match opened {
        Ok(session) => (session, None),
        Err(error) => (requested.unwrap_or_default(), Some(error)),
    };
    Resolved {
        socket,
        session,
        phux,
        error,
        cli: None,
    }
}

/// Run the desktop until its last window closes.
#[must_use]
pub fn main() -> ExitCode {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{}", diag::redact(&format!("{error:#}")));
            return ExitCode::from(2);
        }
    };
    diag::init_logging();
    runtime::configure_explicit(options.phux.is_some() || std::env::var_os("PHUX_BIN").is_some());
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
            eprintln!("{}", diag::redact(&format!("{error:#}")));
            return ExitCode::from(2);
        }
    };
    let restore = options.restores_update();
    let app_options = AppOptions {
        socket: resolved.socket,
        session: resolved.session,
        phux: resolved.phux,
        layout_path: options.layout.clone().unwrap_or_default(),
        fresh: options.fresh,
        initial_terminal,
        quick: options.quick,
        dev_telemetry: options.dev_telemetry
            || std::env::var("PHUX_DESKTOP_DEV_TELEMETRY").as_deref() == Ok("1"),
        startup_error: resolved.error,
        cli_install: resolved.cli,
    };
    // A startup error means no CLI or no server; the HUD then shows no server line.
    let cli = (app_options.startup_error.is_none() && !app_options.socket.as_os_str().is_empty())
        .then(|| crate::devtools::ServerCli {
            phux: app_options.phux.clone(),
            socket: app_options.socket.clone(),
        });
    crate::devtools::init(
        crate::devtools::requested(options.dev),
        crate::devtools::prefs::load(),
        cli,
    );
    let evidence = options.evidence.clone().map(|dir| diag::EvidenceOptions {
        dir,
        exit_after: options.smoke,
    });
    gpui_platform::application()
        .with_assets(crate::assets::Assets)
        .run(move |cx| {
            cx.set_app_identity("dev.phux.desktop", "phux");
            gpui_kit::init(cx);
            diag::install(evidence, cx);
            diag::film::install(cx);
            crate::devtools::install(cx);
            if let Err(error) = app::open_with_restore(app_options, restore, cx) {
                let line = format!("could not open the desktop window: {error:#}");
                log::error!("{line}");
                eprintln!("{}", diag::redact(&line));
                cx.quit();
            }
        });
    crate::devtools::shutdown();
    diag::exit_code()
}

fn parse_terminal(text: &str) -> anyhow::Result<ResourceId> {
    phux_client_ffi::projection::id::parse(text).with_context(|| {
        format!("--terminal {text:?} is not a terminal id (local:N or satellite:HOST:N)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Options> {
        Options::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn explicit_launch_targets_do_not_consume_an_update_restart() {
        for args in [
            vec!["--session", "Chat"],
            vec!["--socket", "/isolated.sock"],
            vec!["--phux", "/isolated/phux"],
            vec!["--layout", "/isolated/layout.json"],
            vec!["--terminal", "local:3"],
            vec!["--evidence", "/isolated/evidence"],
            vec!["--fresh"],
            vec!["--quick"],
            vec!["--dev"],
        ] {
            assert!(!parse(&args).unwrap().restores_update(), "{args:?}");
        }
    }

    #[test]
    fn every_documented_flag_parses() {
        let options = parse(&[
            "--socket",
            "/tmp/s",
            "--session",
            "w",
            "--phux",
            "/bin/phux",
            "--layout",
            "/tmp/l.json",
            "--fresh",
            "--terminal",
            "local:3",
            "--quick",
            "--dev-telemetry",
            "--dev",
            "--evidence",
            "/tmp/e",
            "--smoke",
        ])
        .expect("valid");
        assert_eq!(options.socket.as_deref(), Some(Path::new("/tmp/s")));
        assert_eq!(options.session.as_deref(), Some("w"));
        assert!(options.fresh && options.quick && options.smoke && options.dev_telemetry);
        assert!(options.dev);
        assert_eq!(options.terminal.as_deref(), Some("local:3"));
    }

    #[test]
    fn smoke_without_evidence_and_unknown_flags_are_refused() {
        assert!(parse(&["--smoke"]).is_err());
        assert!(parse(&["--sockets", "/tmp/s"]).is_err());
        assert!(parse(&["--socket"]).is_err());
        assert!(parse(&["--socket", ""]).is_err());
        assert_eq!(
            parse(&["-psn_0_12345"]).expect("finder arg"),
            Options::default()
        );
    }

    /// The CLI is a separate prerequisite: without one the window opens on an
    /// explicit install instruction, and nothing else is attempted.
    #[test]
    fn a_missing_cli_opens_on_the_install_instruction() {
        let options = Options {
            socket: Some("/tmp/phux-desktop-missing-cli.sock".into()),
            session: Some("w".into()),
            ..Options::default()
        };
        let resolved = resolve_with(&options, None);
        assert_eq!(resolved.error.as_deref(), Some(runtime::MISSING));
        assert_eq!(resolved.phux, Path::new("phux"));
        assert_eq!(resolved.session, "w");
        assert_eq!(
            resolved.socket,
            Path::new("/tmp/phux-desktop-missing-cli.sock")
        );
    }

    /// An explicit `--phux` that is not there names the path it could not run.
    #[test]
    fn a_missing_explicit_cli_names_its_path() {
        let missing = PathBuf::from("/nonexistent/phux-desktop-test/phux");
        let options = Options {
            socket: Some("/tmp/phux-desktop-missing-cli.sock".into()),
            phux: Some(missing.clone()),
            ..Options::default()
        };
        let resolved = resolve(&options);
        let error = resolved.error.expect("a startup error");
        assert!(
            error.contains("could not run /nonexistent/phux-desktop-test/phux"),
            "{error}"
        );
        assert_eq!(resolved.phux, missing);
    }

    #[test]
    fn automatic_discovery_keeps_the_command_name_and_records_installed_or_found() {
        let path = PathBuf::from("/Users/ada/.local/bin/phux");
        let mut resolved = Resolved {
            socket: PathBuf::new(),
            session: String::new(),
            phux: path.clone(),
            error: None,
            cli: None,
        };
        note_discovered_cli(
            &mut resolved,
            runtime::Located {
                path: path.clone(),
                installed: true,
            },
            true,
        );
        assert_eq!(resolved.phux, Path::new("phux"));
        let cli = resolved.cli.expect("installed cli");
        assert!(cli.installed);
        assert_eq!(cli.path, path);

        let mut pinned = Resolved {
            socket: PathBuf::new(),
            session: String::new(),
            phux: path.clone(),
            error: None,
            cli: None,
        };
        note_discovered_cli(
            &mut pinned,
            runtime::Located {
                path: path.clone(),
                installed: false,
            },
            false,
        );
        assert_eq!(pinned.phux, path);
        assert!(!pinned.cli.expect("found cli").installed);
    }
}
