//! Machine segments for the sidebar (ADR-0140).
//!
//! The attached server's sessions reach the sidebar live over the attach
//! stream. Every *other* machine phux reaches (this one, when the attach is
//! remote, and each `[[remote]]` host) comes from a hosts provider: a
//! command that prints the `phux.hosts/v1` document
//! ([`phux_core::host_list`]), re-run on a cadence. The built-in provider is
//! this binary's own `ls --all --json`; `[sidebar] hosts-provider` swaps in
//! any command that prints the same shape. A native feature and a plugin
//! feed the strip through one contract, so neither can do something the
//! other cannot.
//!
//! A click on another machine's session does not relay through this
//! server: it replaces this process with `phux attach` against that host
//! ([`exec_switch_host`]), so every dial, repair, and reconnect rule the CLI
//! already has applies unchanged.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use phux_config::SidebarCfg;
use phux_core::host_list::{HOSTS_SCHEMA_VERSION, HostJson, HostKind, HostListJson};
use tokio::sync::mpsc::UnboundedSender;

/// The provider's registry name for this machine (`phux ls --all`'s local
/// row), and the `switch-host` argument that means "this machine".
pub const LOCAL_HOST: &str = "local";

/// Floor on the provider cadence: a provider dials every host, and a
/// misconfigured `refresh-secs = 0` must not become a dial loop.
const MIN_REFRESH: Duration = Duration::from_secs(2);

/// How long one provider run may take before it is killed and skipped. The
/// built-in provider gives each host 3 s concurrently, so this is headroom
/// for a slow third-party provider rather than a tuning knob.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(15);

/// Which machine this process attached to.
///
/// Set once by the CLI before the attach loop starts. One process holds one
/// attach, and a host switch replaces the process, so the value never
/// changes underneath the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachOrigin {
    /// This machine's own server, over its local socket.
    Local,
    /// A registered host, by its `[[remote]]` name.
    Remote(String),
}

static ORIGIN: OnceLock<AttachOrigin> = OnceLock::new();

/// Record which machine this process is attached to. The first call wins;
/// later calls (a graceful-upgrade reconnect re-entering the attach path)
/// carry the same origin and are ignored.
pub fn set_attach_origin(origin: AttachOrigin) {
    let _ = ORIGIN.set(origin);
}

/// The recorded origin, or `None` when no CLI attach recorded one: a test
/// or embedded driver, which runs no provider.
#[must_use]
pub fn recorded_origin() -> Option<AttachOrigin> {
    ORIGIN.get().cloned()
}

/// The last provider answer this process received. A session switch
/// rebuilds the attach loop from empty; seeding from here keeps the other
/// machines' segments on screen instead of blinking out until the next run.
static LAST_KNOWN: std::sync::Mutex<Vec<HostJson>> = std::sync::Mutex::new(Vec::new());

/// Remember a provider answer for the next loop's seed.
pub fn remember(hosts: &[HostJson]) {
    if let Ok(mut last) = LAST_KNOWN.lock() {
        last.clear();
        last.extend_from_slice(hosts);
    }
}

/// The seed for a freshly built attach loop.
#[must_use]
pub fn last_known() -> Vec<HostJson> {
    LAST_KNOWN
        .lock()
        .map(|last| last.clone())
        .unwrap_or_default()
}

impl AttachOrigin {
    /// The provider row name this origin corresponds to.
    #[must_use]
    pub fn host_name(&self) -> &str {
        match self {
            Self::Local => LOCAL_HOST,
            Self::Remote(name) => name,
        }
    }
}

/// The `[sidebar]` hosts keys, folded to what the driver runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostsSettings {
    /// Whether a provider runs at all.
    pub enabled: bool,
    /// Provider argv; empty means this binary's `ls --all --json`.
    pub provider: Vec<String>,
    /// Cadence between runs, already clamped to at least two seconds.
    pub refresh: Duration,
}

impl HostsSettings {
    /// No provider: the headless and test default.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            provider: Vec::new(),
            refresh: MIN_REFRESH,
        }
    }

    /// Fold the `[sidebar]` hosts keys.
    #[must_use]
    pub fn from_cfg(cfg: &SidebarCfg) -> Self {
        Self {
            enabled: cfg.hosts,
            provider: cfg.hosts_provider.clone(),
            refresh: Duration::from_secs(cfg.hosts_refresh_secs).max(MIN_REFRESH),
        }
    }

    /// The program and arguments one provider run executes, or `None` when
    /// the built-in provider is selected but this binary's path is unknown.
    fn argv(&self) -> Option<(PathBuf, Vec<String>)> {
        if let Some((program, args)) = self.provider.split_first() {
            return Some((PathBuf::from(program), args.to_vec()));
        }
        let exe = std::env::current_exe().ok()?;
        let args = ["ls", "--all", "--json"].map(str::to_owned).to_vec();
        Some((exe, args))
    }
}

/// Start the provider loop: run it now, then every `settings.refresh`.
///
/// Each parsed document's hosts go out on `tx`. The loop stops when the
/// receiver is dropped (the attach loop ended). A failed or unparseable run
/// sends nothing and keeps the last good segments on screen.
pub fn spawn_provider(settings: &HostsSettings, tx: UnboundedSender<Vec<HostJson>>) {
    if !settings.enabled {
        return;
    }
    let Some((program, args)) = settings.argv() else {
        tracing::warn!("sidebar hosts provider: this binary's path is unknown; not starting");
        return;
    };
    let refresh = settings.refresh;
    tokio::spawn(async move {
        loop {
            if tx.is_closed() {
                return;
            }
            if let Some(hosts) = run_once(&program, &args).await
                && tx.send(hosts).is_err()
            {
                return;
            }
            tokio::time::sleep(refresh).await;
        }
    });
}

async fn run_once(program: &PathBuf, args: &[String]) -> Option<Vec<HostJson>> {
    let fut = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(PROVIDER_TIMEOUT, fut).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            tracing::warn!(program = %program.display(), %err, "sidebar hosts provider failed to run");
            return None;
        }
        Err(_) => {
            tracing::warn!(program = %program.display(), "sidebar hosts provider timed out");
            return None;
        }
    };
    if !output.status.success() {
        tracing::warn!(program = %program.display(), status = %output.status, "sidebar hosts provider exited non-zero");
        return None;
    }
    parse_document(&output.stdout)
}

/// Parse a provider's stdout. Only the major version this build reads is
/// accepted; added keys are ignored by construction.
#[must_use]
pub fn parse_document(stdout: &[u8]) -> Option<Vec<HostJson>> {
    match serde_json::from_slice::<HostListJson>(stdout) {
        Ok(doc) if doc.schema_version == HOSTS_SCHEMA_VERSION => Some(doc.hosts),
        Ok(doc) => {
            tracing::warn!(
                version = doc.schema_version,
                "sidebar hosts provider printed an unsupported phux.hosts version"
            );
            None
        }
        Err(err) => {
            tracing::warn!(%err, "sidebar hosts provider printed an unparseable document");
            None
        }
    }
}

/// The segments to draw beside the attached server's own: every provider
/// row except the one this process is attached to, which the attach stream
/// already shows live.
#[must_use]
pub fn other_hosts<'a>(hosts: &'a [HostJson], origin: &AttachOrigin) -> Vec<&'a HostJson> {
    hosts
        .iter()
        .filter(|host| !is_origin(host, origin))
        .collect()
}

fn is_origin(host: &HostJson, origin: &AttachOrigin) -> bool {
    match origin {
        AttachOrigin::Local => host.kind == HostKind::Local,
        AttachOrigin::Remote(name) => host.kind == HostKind::Remote && host.name == *name,
    }
}

/// A machine name as the hosts provider labels this machine: the first
/// label of a DNS name (`mac.local` is `mac`, as `phux ls --all` prints
/// it), and an IP address or bare name unchanged.
#[must_use]
pub fn short_host_label(host: &str) -> &str {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    host.split('.')
        .next()
        .filter(|first| !first.is_empty())
        .unwrap_or(host)
}

/// The label for the attached server's own segment: the registry name of a
/// remote origin, else the provider's name for this machine, else the
/// serving host the server reported.
#[must_use]
pub fn origin_label(
    hosts: &[HostJson],
    origin: &AttachOrigin,
    serving_host: Option<&str>,
) -> Option<String> {
    match origin {
        AttachOrigin::Remote(name) => Some(name.clone()),
        AttachOrigin::Local => hosts
            .iter()
            .find(|host| host.kind == HostKind::Local)
            .map(|host| host.label.clone())
            .or_else(|| serving_host.map(str::to_owned)),
    }
}

/// The argv `phux attach` needs to land on `session` at `host`.
///
/// [`LOCAL_HOST`] pins the local socket with `--socket`, which also stops a
/// session that happens to share a registered host's name from being read
/// as that host.
#[must_use]
pub fn switch_args(host: &str, session: &str) -> Vec<String> {
    let mut args = vec!["attach".to_owned()];
    if host == LOCAL_HOST {
        args.push("--socket".to_owned());
        args.push(
            phux_config::socket::default_socket_path()
                .to_string_lossy()
                .into_owned(),
        );
    } else {
        args.push("--remote".to_owned());
        args.push(host.to_owned());
    }
    args.push(session.to_owned());
    args
}

/// Become `phux attach` against `host`'s `session`.
///
/// The caller (the attach driver) has already restored the terminal, so a
/// failed exec leaves a usable shell with the reason printed. On success
/// this never returns: the new process owns the terminal and draws its own
/// first frame.
#[allow(
    clippy::print_stderr,
    reason = "the TUI has already handed the terminal back; a failed exec \
              reports on the cooked terminal, like a detach explanation"
)]
pub fn exec_switch_host(host: &str, session: &str) -> ! {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("phux"));
    let err = std::process::Command::new(&exe)
        .args(switch_args(host, session))
        .exec();
    eprintln!("phux: could not switch to {session} on {host}: {err}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_core::session_list::SessionJson;

    /// The sidebar's machine header read "phalls-Mac-mini.local" until the
    /// hosts listing landed and renamed it "phalls-Mac-mini".
    #[test]
    fn a_served_host_is_labelled_as_the_hosts_provider_labels_it() {
        assert_eq!(short_host_label("mac.local"), "mac");
        assert_eq!(short_host_label("mini"), "mini");
        assert_eq!(short_host_label("10.0.0.7"), "10.0.0.7");
        assert_eq!(short_host_label("::1"), "::1");
        assert_eq!(short_host_label(".weird"), ".weird");
    }

    fn host(name: &str, kind: HostKind) -> HostJson {
        HostJson {
            name: name.to_owned(),
            label: format!("{name}-label"),
            kind,
            endpoint: None,
            reachable: true,
            error: None,
            sessions: vec![SessionJson {
                name: "s".to_owned(),
                windows: 1,
                attached: false,
                attached_clients: 0,
                keep_empty: false,
                empty: false,
            }],
        }
    }

    #[test]
    fn the_attached_machine_is_never_a_second_segment() {
        let hosts = vec![
            host(LOCAL_HOST, HostKind::Local),
            host("mini", HostKind::Remote),
            host("xps", HostKind::Remote),
        ];
        let names = |origin| {
            other_hosts(&hosts, &origin)
                .iter()
                .map(|h| h.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(AttachOrigin::Local), ["mini", "xps"]);
        assert_eq!(
            names(AttachOrigin::Remote("mini".to_owned())),
            [LOCAL_HOST, "xps"]
        );
    }

    #[test]
    fn origin_label_prefers_the_registry_name_then_this_machine() {
        let hosts = vec![host(LOCAL_HOST, HostKind::Local)];
        assert_eq!(
            origin_label(
                &hosts,
                &AttachOrigin::Remote("mini".to_owned()),
                Some("Mac")
            ),
            Some("mini".to_owned())
        );
        assert_eq!(
            origin_label(&hosts, &AttachOrigin::Local, Some("Mac")),
            Some("local-label".to_owned())
        );
        assert_eq!(
            origin_label(&[], &AttachOrigin::Local, Some("Mac")),
            Some("Mac".to_owned())
        );
    }

    #[test]
    fn switch_args_pin_the_local_socket_and_name_remote_hosts() {
        let local = switch_args(LOCAL_HOST, "work");
        assert_eq!(local[..2], ["attach", "--socket"]);
        assert_eq!(local.last().map(String::as_str), Some("work"));
        assert_eq!(
            switch_args("mini", "phall"),
            ["attach", "--remote", "mini", "phall"]
        );
    }

    #[test]
    fn a_foreign_major_version_is_refused() {
        assert!(parse_document(br#"{"schema_version":2,"hosts":[]}"#).is_none());
        assert!(parse_document(b"not json").is_none());
        let ok = parse_document(br#"{"schema_version":1,"hosts":[]}"#);
        assert_eq!(ok, Some(Vec::new()));
    }

    #[test]
    fn refresh_is_clamped() {
        let cfg = SidebarCfg {
            hosts_refresh_secs: 0,
            ..SidebarCfg::default()
        };
        assert_eq!(HostsSettings::from_cfg(&cfg).refresh, MIN_REFRESH);
    }
}
