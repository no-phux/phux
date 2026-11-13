//! Command line, the installed CLI, and the server socket.
//!
//! The client starts or reuses the server through `phux server --ensure`,
//! then asks the CLI which socket that server listens on. Those calls can block for seconds, so [`resolve`] only
//! reads flags and the disk; [`Resolved::ensured`] runs on a worker.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const USAGE: &str = "usage: phux-rune [--socket PATH] [--session NAME] [--phux PATH] [--phone]";

/// The line that installs the CLI, for the window that can't find one.
pub const INSTALL: &str = "curl -fsSL https://phux.sh/install | sh";

/// The script `INSTALL` downloads, as this build was made with.
const INSTALLER: &str = include_str!("../../../scripts/install.sh");

/// Install the CLI, and on macOS phux Server.app, the way `INSTALL` would:
/// the same verified download, `~/.local/bin`, and shell profile line.
/// Blocks for as long as the download takes.
pub fn install() -> Result<(), String> {
    run_installer(INSTALLER)
}

/// Run `script` as `sh -c` and, when it fails, say why in its last words.
fn run_installer(script: &str) -> Result<(), String> {
    let out = Command::new("/bin/sh")
        .args(["-c", script, "install.sh"])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run the installer: {error}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let last = stderr.lines().rfind(|line| !line.trim().is_empty());
    Err(last
        .map(|line| line.trim_start_matches("phux: error: ").trim().to_owned())
        .unwrap_or_else(|| format!("The installer stopped ({}).", out.status)))
}

#[derive(Debug, Default)]
pub struct Options {
    pub socket: Option<PathBuf>,
    pub session: Option<String>,
    pub phux: Option<PathBuf>,
    /// The phone form: a phone-sized window with a drawn keyboard.
    pub phone: bool,
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
                "--phone" => options.phone = true,
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
    /// The server's socket once known, else why not.
    pub socket: Result<PathBuf, String>,
    pub session: Option<String>,
    /// The CLI, for its registry and its updater.
    pub phux: Option<PathBuf>,
    /// A flag or the environment chose the server: this run belongs to
    /// development, so it neither updates phux nor reopens a remembered
    /// machine.
    pub aimed: bool,
    /// The socket a flag or the environment named. `socket` holds the last
    /// answer, which may be a failure; this never stops being the address.
    pinned: Option<PathBuf>,
    /// `--phux`, kept for searching again.
    named_phux: Option<PathBuf>,
    embedded: bool,
}

impl Resolved {
    /// A run with no server, for tests that only want a window's state.
    #[cfg(test)]
    pub(crate) fn offline(why: &str) -> Self {
        Self {
            socket: Err(why.to_owned()),
            session: None,
            phux: None,
            aimed: true,
            pinned: None,
            named_phux: None,
            embedded: false,
        }
    }

    /// `--socket` alone means "that server, as it is": only a CLI named
    /// beside it may start one there.
    pub const fn can_ensure(&self) -> bool {
        !self.embedded && (self.pinned.is_none() || self.named_phux.is_some())
    }

    /// Whether the window has nothing to run `phux server --ensure` with.
    pub const fn missing_cli(&self) -> bool {
        self.phux.is_none() && self.can_ensure()
    }

    /// Search for the CLI again, then start or reuse the server. Blocks.
    pub fn ensured(mut self) -> Self {
        if self.embedded {
            return self;
        }
        let phux = locate(self.named_phux.as_deref());
        self.phux = phux.as_ref().ok().cloned();
        if self.can_ensure() {
            let pinned = self.pinned.as_deref();
            self.socket = phux.and_then(|phux| ensure_server(&phux, pinned));
        }
        self
    }

    /// A platform embedder supplies its remote target directly, without a
    /// local CLI, process spawning, registry discovery or self-update.
    pub fn embedded(session: Option<String>) -> Self {
        Self {
            socket: Err("Native mobile connects directly to a remote host.".into()),
            session,
            phux: None,
            aimed: true,
            pinned: None,
            named_phux: None,
            embedded: true,
        }
    }
}

/// What this launch was told, and the CLI if one is on disk. Never runs it.
pub fn resolve(options: &Options) -> Resolved {
    let session = options
        .session
        .clone()
        .or_else(|| std::env::var("PHUX_SESSION").ok().filter(|s| !s.is_empty()));
    let explicit = options
        .socket
        .clone()
        .or_else(|| std::env::var_os("PHUX_SOCKET").map(PathBuf::from));
    let phux = locate(options.phux.as_deref());
    let aimed = explicit.is_some()
        || options.phux.is_some()
        || ["PHUX_BIN", "PHUX_PROFILE"]
            .iter()
            .any(|key| std::env::var_os(key).is_some());
    let pinned = explicit.clone();
    let socket = match explicit {
        Some(socket) => Ok(socket),
        None => phux
            .clone()
            .and_then(|_| Err("The server hasn't been asked yet.".to_owned())),
    };
    Resolved {
        socket,
        session,
        phux: phux.ok(),
        aimed,
        pinned,
        named_phux: options.phux.clone(),
        embedded: false,
    }
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

/// How long a CLI call may take before the window shows the failure.
const CLI_TIMEOUT: Duration = Duration::from_secs(30);

/// The most a CLI answer may print.
const MAX_OUTPUT: u64 = 16 << 20;

/// How long a finished CLI's pipes get to reach their end.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

fn run(phux: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    run_for(phux, args, CLI_TIMEOUT)
}

pub fn run_for(phux: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>, String> {
    let mut child = Command::new(phux)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run {}: {error}", phux.display()))?;
    // Drain both pipes on threads so a chatty CLI can't block on a full pipe
    // while this waits for it. Past the limit the pipe closes, and the writer
    // gets SIGPIPE instead of this window's memory.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (done, bytes) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(MAX_OUTPUT + 1).read_to_end(&mut bytes);
            }
            let _ = done.send(bytes);
        });
        bytes
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`phux {}` did not finish in {} s; your sessions were not changed.",
                    args.join(" "),
                    timeout.as_secs()
                ));
            }
            Err(error) => return Err(format!("waiting for {}: {error}", phux.display())),
        }
    };
    // A daemon the CLI left behind (an ssh master, say) can hold a pipe open
    // long after the CLI is gone, so its end of file can't be waited for.
    let stdout = stdout.recv_timeout(DRAIN_GRACE).unwrap_or_default();
    let stderr = stderr.recv_timeout(DRAIN_GRACE).unwrap_or_default();
    if stdout.len() as u64 > MAX_OUTPUT {
        return Err(format!(
            "`phux {}` printed more than {} MB.",
            args.join(" "),
            MAX_OUTPUT >> 20
        ));
    }
    if status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "`phux {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&stderr).trim()
        ))
    }
}

/// `socket` keeps a named server named; without it the CLI picks its own.
fn ensure_server(phux: &Path, socket: Option<&Path>) -> Result<PathBuf, String> {
    let mut at: Vec<&str> = Vec::new();
    if let Some(socket) = socket {
        at = vec![
            "--socket",
            socket.to_str().ok_or("socket path isn't UTF-8")?,
        ];
    }
    let call = |args: &[&'static str]| run(phux, &[at.as_slice(), args].concat());
    // The window picks its own session (README: it never creates one), so a
    // server started here seeds none. A CLI older than `--no-seed` refuses
    // the word; it seeds as it always did.
    if let Err(error) = call(&["server", "--ensure", "--no-seed"]) {
        if !error.contains("unexpected argument '--no-seed'") {
            return Err(error);
        }
        call(&["server", "--ensure"])?;
    }
    let status: serde_json::Value = serde_json::from_slice(&call(&["status", "--json"])?)
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
            "--phone",
        ])
        .expect("valid flags");
        assert_eq!(options.socket, Some(PathBuf::from("/tmp/s")));
        assert_eq!(options.session.as_deref(), Some("work"));
        assert_eq!(options.phux, Some(PathBuf::from("/bin/phux")));
        assert!(options.phone);
        assert!(!parse(&[]).expect("no flags").phone);
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

    fn launch(socket: Option<&str>, phux: Option<&str>) -> Resolved {
        Resolved {
            socket: socket.map_or_else(|| Err("not yet".to_owned()), |s| Ok(s.into())),
            session: None,
            phux: phux.map(PathBuf::from),
            aimed: false,
            pinned: socket.map(PathBuf::from),
            named_phux: phux.filter(|_| socket.is_some()).map(PathBuf::from),
            embedded: false,
        }
    }

    #[test]
    fn a_named_socket_is_left_alone_unless_a_cli_came_with_it() {
        assert!(launch(None, None).can_ensure());
        assert!(!launch(Some("/tmp/s"), None).can_ensure());
        assert!(launch(Some("/tmp/s"), Some("/bin/phux")).can_ensure());
    }

    #[test]
    fn only_a_window_that_needs_the_cli_misses_it() {
        assert!(launch(None, None).missing_cli());
        assert!(!launch(None, Some("/bin/phux")).missing_cli());
        assert!(!launch(Some("/tmp/s"), None).missing_cli());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_passes_a_named_socket_to_every_call() {
        let dir = std::env::temp_dir().join(format!("phux-rune-ensure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let phux = dir.join("phux");
        std::fs::write(
            &phux,
            "#!/bin/sh\n[ \"$1\" = --socket ] || exit 9\nprintf '{\"socket\":\"%s\"}' \"$2\"\n",
        )
        .unwrap();
        Command::new("/bin/chmod")
            .arg("+x")
            .arg(&phux)
            .status()
            .unwrap();
        let found = ensure_server(&phux, Some(Path::new("/tmp/named"))).expect("ensured");
        assert_eq!(found, PathBuf::from("/tmp/named"));
        assert!(ensure_server(&phux, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_asks_for_no_seed_and_falls_back_on_an_older_cli() {
        let dir = std::env::temp_dir().join(format!("phux-rune-seed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("calls");
        let phux = dir.join("phux");
        // An older CLI: `--no-seed` is an unknown argument; `status` answers.
        std::fs::write(
            &phux,
            format!(
                "#!/bin/sh\necho \"$*\" >> {log}\ncase \"$*\" in\n*--no-seed*) echo \"error: unexpected argument '--no-seed' found\" >&2; exit 2;;\n*status*) printf '{{\"socket\":\"/tmp/s\"}}';;\nesac\n",
                log = log.display()
            ),
        )
        .unwrap();
        Command::new("/bin/chmod")
            .arg("+x")
            .arg(&phux)
            .status()
            .unwrap();
        let found = ensure_server(&phux, Some(Path::new("/tmp/s"))).expect("ensured");
        assert_eq!(found, PathBuf::from("/tmp/s"));
        let calls = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = calls.lines().collect();
        assert_eq!(lines[0], "--socket /tmp/s server --ensure --no-seed");
        assert_eq!(lines[1], "--socket /tmp/s server --ensure");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_ensure_keeps_aiming_at_the_named_socket() {
        let dir = std::env::temp_dir().join(format!("phux-rune-pinned-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (phux, log) = (dir.join("phux"), dir.join("log"));
        std::fs::write(
            &phux,
            format!("#!/bin/sh\necho \"$1\" >> {}\nexit 1\n", log.display()),
        )
        .unwrap();
        Command::new("/bin/chmod")
            .arg("+x")
            .arg(&phux)
            .status()
            .unwrap();
        let named = launch(Some("/tmp/named"), phux.to_str());
        let failed = named.ensured();
        assert!(failed.socket.is_err());
        // The retry a window makes after that failure.
        assert!(failed.ensured().socket.is_err());
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(calls, "--socket\n--socket\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_install_says_why_in_the_installer_s_last_words() {
        assert_eq!(run_installer("exit 0"), Ok(()));
        let why = "printf 'phux: info: fetching\\nphux: error: no release\\n\\n' >&2; exit 1";
        assert_eq!(run_installer(why), Err("no release".to_owned()));
        assert!(run_installer("exit 3").expect_err("fails").contains('3'));
    }

    #[cfg(unix)]
    #[test]
    fn run_returns_stdout_and_reports_stderr() {
        let sh = Path::new("/bin/sh");
        assert_eq!(run(sh, &["-c", "printf ok"]).expect("runs"), b"ok");
        let error = run(sh, &["-c", "echo nope >&2; exit 3"]).expect_err("fails");
        assert!(error.contains("nope"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn endless_output_fails_instead_of_filling_memory() {
        let error = run(Path::new("/bin/sh"), &["-c", "exec cat /dev/zero"]).expect_err("capped");
        assert!(error.contains("more than"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_daemon_holding_the_pipes_doesnt_hang_the_call() {
        let started = Instant::now();
        let out = run(Path::new("/bin/sh"), &["-c", "sleep 30 & exit 0"]).expect("finishes");
        assert!(out.is_empty());
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
