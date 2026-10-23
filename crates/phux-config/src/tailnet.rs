//! The local Tailscale client's view of this machine's tailnet: whether it is
//! signed in and connected, and which other machines are on it.
//!
//! Read from `tailscale status --json` through the CLI
//! [`crate::overlay::tailscale_cli`] finds. Discovery only lists what the
//! tailnet already knows; registering a machine stays `phux host add`.
//! [`sign_in`] starts the client's own browser sign-in, or brings a stopped
//! client back up, and never touches its settings.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::overlay::{run_tailscale_capture, tailscale_cli};

/// How long `status --json` may take: it lists every peer.
const STATUS_DEADLINE: Duration = Duration::from_secs(5);

/// How long `tailscale up` may take to reconnect a stopped client.
const UP_DEADLINE: Duration = Duration::from_secs(20);

/// How long [`sign_in`] waits for a sign-in link or for the client to
/// come up before it reports what it has.
const SIGN_IN_WAIT: Duration = Duration::from_secs(15);

/// How long a started `tailscale login` waits for the browser before it
/// gives up on its own.
const LOGIN_TIMEOUT: &str = "15m";

/// Where the local Tailscale client stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailnetState {
    /// Connected; peers are listed.
    Running,
    /// Starting up or connecting.
    Starting,
    /// Signed in but disconnected (`tailscale down`, or the app's switch).
    Stopped,
    /// Not signed in; [`sign_in`] starts a browser sign-in.
    NeedsLogin,
    /// Signed in, waiting for an admin to approve this device.
    NeedsMachineAuth,
    /// The CLI is installed but its daemon or app does not answer.
    NotRunning,
    /// No Tailscale CLI was found.
    NotInstalled,
}

impl TailnetState {
    /// The stable spelling `phux host discover --json` reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Starting => "starting",
            Self::Stopped => "stopped",
            Self::NeedsLogin => "needs_login",
            Self::NeedsMachineAuth => "needs_machine_auth",
            Self::NotRunning => "not_running",
            Self::NotInstalled => "not_installed",
        }
    }

    fn from_backend(state: &str) -> Self {
        match state {
            "Running" => Self::Running,
            "Starting" => Self::Starting,
            "Stopped" => Self::Stopped,
            "NeedsLogin" => Self::NeedsLogin,
            "NeedsMachineAuth" => Self::NeedsMachineAuth,
            // `NoState` and anything newer: the daemon answered but is not
            // usable yet.
            _ => Self::NotRunning,
        }
    }
}

/// Another machine on the tailnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetPeer {
    /// The `MagicDNS` short name (`mini`), or the host name when the tailnet
    /// serves no DNS name for it.
    pub name: String,
    /// The fully qualified `MagicDNS` name, without the trailing dot.
    pub dns_name: Option<String>,
    /// The host name the machine reports for itself.
    pub host_name: String,
    /// The operating system Tailscale reports: `linux`, `macOS`, `iOS`, ...
    pub os: String,
    /// Whether the coordination server sees it connected.
    pub online: bool,
    /// Its tailnet addresses, IPv4 first as Tailscale lists them.
    pub addresses: Vec<IpAddr>,
    /// Whether it runs Tailscale SSH (it advertises SSH host keys).
    pub tailscale_ssh: bool,
}

impl TailnetPeer {
    /// Whether phux ships for this machine's operating system.
    #[must_use]
    pub fn supported(&self) -> bool {
        matches!(self.os.as_str(), "linux" | "macOS")
    }

    /// What to `ssh` to: the `MagicDNS` name, else the first address.
    #[must_use]
    pub fn ssh_host(&self) -> String {
        self.dns_name.clone().unwrap_or_else(|| {
            self.addresses
                .first()
                .map_or_else(|| self.name.clone(), ToString::to_string)
        })
    }

    /// Whether `host` (a DNS name, short name or address) names this peer.
    #[must_use]
    pub fn is_named_by(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return false;
        }
        host == self.name.to_ascii_lowercase()
            || self.dns_name.as_deref() == Some(host.as_str())
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| self.addresses.contains(&address))
    }
}

/// The local client's view of the tailnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tailnet {
    /// Where the client stands.
    pub state: TailnetState,
    /// The CLI that answered, when one was found.
    pub cli: Option<PathBuf>,
    /// The sign-in link of a sign-in in progress.
    pub auth_url: Option<String>,
    /// The tailnet's name (often the account's login).
    pub name: Option<String>,
    /// This machine's `MagicDNS` name.
    pub self_name: Option<String>,
    /// Every other machine, connected ones first, then by name.
    pub peers: Vec<TailnetPeer>,
}

impl Tailnet {
    const fn without_client(state: TailnetState, cli: Option<PathBuf>) -> Self {
        Self {
            state,
            cli,
            auth_url: None,
            name: None,
            self_name: None,
            peers: Vec::new(),
        }
    }
}

/// Read the tailnet from the local client, best effort: every failure is a
/// [`TailnetState`], never an error.
#[must_use]
pub fn discover() -> Tailnet {
    let Some(cli) = tailscale_cli() else {
        return Tailnet::without_client(TailnetState::NotInstalled, None);
    };
    discover_with(cli)
}

fn discover_with(cli: PathBuf) -> Tailnet {
    match run_tailscale_capture(cli.as_os_str(), &["status", "--json"], STATUS_DEADLINE) {
        // A spawn failure means the configured program is not there.
        None if !cli.exists() => Tailnet::without_client(TailnetState::NotInstalled, None),
        None => Tailnet::without_client(TailnetState::NotRunning, Some(cli)),
        Some((_, stdout)) => parse_status(&stdout, cli),
    }
}

/// Parse `tailscale status --json`. A logged-out client still prints its
/// document (and exits non-zero on some versions); output that is not one
/// means the daemon did not answer.
fn parse_status(out: &str, cli: PathBuf) -> Tailnet {
    let Ok(status) = serde_json::from_str::<serde_json::Value>(out) else {
        return Tailnet::without_client(TailnetState::NotRunning, Some(cli));
    };
    let text = |pointer: &str| {
        status
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let state = text("/BackendState")
        .as_deref()
        .map_or(TailnetState::NotRunning, TailnetState::from_backend);
    let running = state == TailnetState::Running;
    let mut peers: Vec<TailnetPeer> = if running {
        status
            .get("Peer")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flat_map(|peers| peers.values())
            .filter_map(parse_peer)
            .collect()
    } else {
        Vec::new()
    };
    peers.sort_by(|a, b| b.online.cmp(&a.online).then_with(|| a.name.cmp(&b.name)));
    Tailnet {
        state,
        cli: Some(cli),
        auth_url: text("/AuthURL").filter(|url| url.starts_with("https://")),
        name: running.then(|| text("/CurrentTailnet/Name")).flatten(),
        self_name: running
            .then(|| text("/Self/DNSName"))
            .flatten()
            .map(|name| name.trim_end_matches('.').to_ascii_lowercase()),
        peers,
    }
}

/// One `Peer` entry. Mullvad exit nodes (they carry a `Location`) are not
/// machines anyone sets up.
fn parse_peer(peer: &serde_json::Value) -> Option<TailnetPeer> {
    if peer
        .get("Location")
        .is_some_and(|location| !location.is_null())
    {
        return None;
    }
    let dns_name = peer
        .get("DNSName")
        .and_then(serde_json::Value::as_str)
        .map(|name| name.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|name| !name.is_empty());
    let host_name = peer
        .get("HostName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let name = dns_name
        .as_deref()
        .and_then(|name| name.split('.').next())
        .map_or_else(|| host_name.clone(), str::to_owned);
    if name.is_empty() {
        return None;
    }
    let addresses = peer
        .get("TailscaleIPs")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|ip| ip.as_str()?.parse::<IpAddr>().ok())
        .collect();
    Some(TailnetPeer {
        name,
        dns_name,
        host_name,
        os: peer
            .get("OS")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        online: peer
            .get("Online")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        addresses,
        tailscale_ssh: peer
            .get("sshHostKeys")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|keys| !keys.is_empty()),
    })
}

/// Why [`sign_in`] could not move the client forward, with what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInError {
    /// What went wrong.
    pub message: String,
    /// What the person can do about it.
    pub remedy: String,
}

impl SignInError {
    fn new(message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            remedy: remedy.into(),
        }
    }
}

/// Move the local client toward `Running`, then report where it stands.
///
/// Open a macOS app that is not running, reconnect a stopped client with
/// `tailscale up`, or start a browser sign-in with `tailscale login` and
/// return once its link is known. The login keeps running in the
/// background until the browser finishes it (or it times out), so a caller
/// shows [`Tailnet::auth_url`] and polls [`discover`].
///
/// # Errors
///
/// When there is no Tailscale to sign in to, or the client refuses.
pub fn sign_in() -> Result<Tailnet, SignInError> {
    sign_in_with(tailscale_cli().as_ref(), open_macos_app)
}

fn sign_in_with(
    cli: Option<&PathBuf>,
    open_app: impl FnOnce() -> bool,
) -> Result<Tailnet, SignInError> {
    let read = || {
        cli.cloned().map_or_else(
            || Tailnet::without_client(TailnetState::NotInstalled, None),
            discover_with,
        )
    };
    let mut tailnet = read();
    if tailnet.state == TailnetState::NotRunning && open_app() {
        tailnet = wait_for(&read, |tailnet| tailnet.state != TailnetState::NotRunning);
    }
    let Some(cli) = tailnet.cli.clone() else {
        return Err(SignInError::new(
            "Tailscale is not installed on this machine",
            "install it from https://tailscale.com/download, then sign in",
        ));
    };
    match tailnet.state {
        TailnetState::Running | TailnetState::Starting | TailnetState::NeedsMachineAuth => {
            Ok(tailnet)
        }
        TailnetState::NotInstalled | TailnetState::NotRunning => Err(SignInError::new(
            "the Tailscale client is installed but not running",
            "start the Tailscale app (or `sudo systemctl start tailscaled` on Linux), then try again",
        )),
        TailnetState::Stopped => {
            let up = run_tailscale_capture(cli.as_os_str(), &["up"], UP_DEADLINE);
            if !matches!(up, Some((true, _))) {
                return Err(SignInError::new(
                    "Tailscale is signed in but disconnected, and `tailscale up` did not reconnect it",
                    "connect from the Tailscale app or menu bar, then try again",
                ));
            }
            Ok(wait_for(&read, |tailnet| {
                tailnet.state != TailnetState::Stopped
            }))
        }
        TailnetState::NeedsLogin if tailnet.auth_url.is_some() => Ok(tailnet),
        TailnetState::NeedsLogin => login(&cli, &read),
    }
}

/// Distinguishes the login logs of sign-ins in one process.
static LOGINS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Start `tailscale login` and wait for its link. A login that exits
/// before there is one (no permission to operate tailscaled, a refused
/// control server) is an error carrying what it said.
fn login(cli: &std::path::Path, read: &dyn Fn() -> Tailnet) -> Result<Tailnet, SignInError> {
    let refused = |detail: String| {
        SignInError::new(
            format!("`tailscale login` failed: {detail}"),
            "sign in from the Tailscale app, or run `tailscale login` in a terminal to see why",
        )
    };
    let log = std::env::temp_dir().join(format!(
        "phux-tailscale-login-{}-{}.log",
        std::process::id(),
        LOGINS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut child = start_login(cli, &log).map_err(|error| refused(error.to_string()))?;
    let deadline = std::time::Instant::now() + SIGN_IN_WAIT;
    let outcome = loop {
        let tailnet = read();
        if tailnet.auth_url.is_some() || tailnet.state != TailnetState::NeedsLogin {
            break Ok(tailnet);
        }
        if let Ok(Some(status)) = child.try_wait()
            && !status.success()
        {
            let said = std::fs::read_to_string(&log).unwrap_or_default();
            let said = said.trim();
            break Err(refused(if said.is_empty() {
                status.to_string()
            } else {
                said.to_owned()
            }));
        }
        if std::time::Instant::now() >= deadline {
            break Ok(tailnet);
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    // The login keeps the file open while it waits for the browser.
    let _ = std::fs::remove_file(&log);
    outcome
}

/// Start `tailscale login` in its own process group, so it outlives this
/// process and whatever started it, and finishes when the browser does.
/// Its output goes to `log`, read only if it fails early.
fn start_login(
    cli: &std::path::Path,
    log: &std::path::Path,
) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt as _;
    let output = std::fs::File::create(log)?;
    std::process::Command::new(cli)
        .args(["login", "--timeout", LOGIN_TIMEOUT])
        .stdin(std::process::Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .process_group(0)
        .spawn()
}

/// Open the macOS Tailscale app when it is installed: it hosts the client.
fn open_macos_app() -> bool {
    cfg!(target_os = "macos")
        && std::path::Path::new("/Applications/Tailscale.app").exists()
        && std::process::Command::new("/usr/bin/open")
            .args(["-g", "-a", "Tailscale"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

/// Re-read until `done` holds or [`SIGN_IN_WAIT`] passes; the last reading
/// either way.
fn wait_for(read: &dyn Fn() -> Tailnet, done: impl Fn(&Tailnet) -> bool) -> Tailnet {
    let deadline = std::time::Instant::now() + SIGN_IN_WAIT;
    loop {
        let tailnet = read();
        if done(&tailnet) || std::time::Instant::now() >= deadline {
            return tailnet;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests;
