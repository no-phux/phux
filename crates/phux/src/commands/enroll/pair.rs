//! The legacy pairing path: the pair document a remote prints, the endpoints
//! it names, the token it leaves behind, and the QUIC probe that picks one.

use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_client::attach::connection::Connection;
use phux_dial::TlsClientIdentity;

// Default QUIC port for an enrolled server (ADR-0081), the one
// `docs/remote-access.md` uses throughout.
use phux_client_runtime::target::DEFAULT_QUIC_PORT;

use super::events::EnrollFailure;
use super::ssh::{authority, ssh_run};

/// How long one direct-route probe may take; only a filtered path waits
/// this long.
pub(super) const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// Test seam for [`PROBE_DEADLINE`], in milliseconds. Undocumented for
/// operators: shortening it turns a slow host into an `ssh://` entry.
pub(super) const PROBE_DEADLINE_ENV: &str = "PHUX_DIRECT_PROBE_TIMEOUT_MS";

/// What `phux pair --json` reported on the remote host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairReport {
    pub(crate) token: String,
    pub(crate) cert_fingerprint: Option<String>,
    /// The CA the far server's certificate chains to (ADR-0153), canonical;
    /// `None` from a far `phux` that predates it or a self-signed leaf.
    pub(crate) ca_fingerprint: Option<String>,
    pub(crate) overlay_addresses: Vec<String>,
}

impl PairReport {
    /// The CA pin the report carries, for probing the routes it names.
    pub(crate) fn authority_pin(&self) -> phux_client_runtime::target::AuthorityPin {
        phux_client_runtime::target::AuthorityPin {
            ca: self.ca_fingerprint.clone(),
            route: None,
        }
    }

    /// Parse `phux pair --json`: tolerant of unknown fields, strict about the two
    /// it needs.
    pub(crate) fn parse(stdout: &str) -> Result<Self, String> {
        let value = pair_document_in(stdout)
            .ok_or_else(|| "remote `phux pair --json` emitted no JSON document".to_owned())?;

        let token = value
            .get("token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| "remote `phux pair --json` reported no token".to_owned())?
            .to_owned();

        let cert_fingerprint = value
            .get("cert_fingerprint")
            .and_then(serde_json::Value::as_str)
            .filter(|fp| !fp.is_empty())
            .map(str::to_owned);

        let overlay_addresses = value
            .get("overlay_addresses")
            .and_then(serde_json::Value::as_array)
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        // A malformed value pins nothing rather than failing the pairing.
        let ca_fingerprint = value
            .get("ca_fingerprint")
            .and_then(serde_json::Value::as_str)
            .and_then(phux_config::known_authorities::canonical_authority);

        Ok(Self {
            token,
            cert_fingerprint,
            ca_fingerprint,
            overlay_addresses,
        })
    }
}

/// The `phux pair --json` object wherever it sits in stdout (pretty-printed,
/// possibly after shell startup noise). Only an object with a `token` key
/// counts.
pub(super) fn pair_document_in(stdout: &str) -> Option<serde_json::Value> {
    json_document_in(stdout, |value| value.get("token").is_some())
}

/// The JSON object `is_document` accepts wherever it sits in a remote
/// command's stdout: the whole output, a one-line document after banner
/// lines, or a pretty-printed one between the outermost braces.
pub(super) fn json_document_in(
    stdout: &str,
    is_document: impl Fn(&serde_json::Value) -> bool,
) -> Option<serde_json::Value> {
    let is_document = |value: &serde_json::Value| is_document(value);
    if let Some(value) = serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .filter(is_document)
    {
        return Some(value);
    }
    // One-line document after banner lines.
    if let Some(value) = stdout
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .filter(is_document)
        })
    {
        return Some(value);
    }
    // Pretty-printed document after banner lines: the outermost braces.
    let start = stdout.find('{')?;
    let end = stdout.rfind('}')?;
    serde_json::from_str::<serde_json::Value>(stdout.get(start..=end)?)
        .ok()
        .filter(is_document)
}

/// The direct routes worth dialing, most likely first: an operator
/// `--endpoint` alone, otherwise each overlay address then the host `ssh -G`
/// names, on the QUIC port, and only when a fingerprint exists (ADR-0031
/// refuses an unpinned routable dial).
pub(crate) fn candidate_endpoints(
    ssh_target_host: &str,
    report: &PairReport,
    host_override: Option<&str>,
    quic_port: u16,
) -> Vec<String> {
    if let Some(host) = host_override {
        return vec![if host.contains("://") {
            host.to_owned()
        } else {
            format!("quic://{host}")
        }];
    }
    if report.cert_fingerprint.is_none() {
        return Vec::new();
    }
    let mut candidates: Vec<String> = report
        .overlay_addresses
        .iter()
        .map(|addr| format!("quic://{}", authority(addr, quic_port)))
        .collect();
    if !ssh_target_host.is_empty() {
        let via_ssh_host = format!("quic://{}", authority(ssh_target_host, quic_port));
        if !candidates.contains(&via_ssh_host) {
            candidates.push(via_ssh_host);
        }
    }
    candidates
}

/// The local path a remote's token is written to.
pub(crate) fn token_path(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join("remotes").join(format!("{name}.token"))
}

/// The local path a satellite's token is written to; `remotes/` and
/// `satellites/` mirror the split registries.
pub(crate) fn satellite_token_path(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join("satellites").join(format!("{name}.token"))
}

/// `phux pair --json` on the host. When `replace_token` is set, the remote
/// mint revokes that bearer in the same rewrite. A pre-versioning token store
/// refuses the mint, and that refusal is reported as it is: its migration is
/// retired (ADR-0179).
pub(super) fn pair_over_ssh(
    ssh_host: &str,
    remote_phux: &str,
    replace_token: Option<&str>,
) -> Result<PairReport, EnrollFailure> {
    let mut pair_argv = vec![remote_phux, "pair", "--json"];
    let replace_flag;
    let replace_value;
    if let Some(token) = replace_token {
        replace_flag = "--replace-token".to_owned();
        replace_value = token.to_owned();
        pair_argv.push(&replace_flag);
        pair_argv.push(&replace_value);
    }
    let paired = ssh_run(ssh_host, &pair_argv).map_err(EnrollFailure::Pair)?;
    if !paired.status.success() {
        return Err(EnrollFailure::Pair(
            paired.command_failure(ssh_host, &pair_argv),
        ));
    }
    PairReport::parse(&paired.stdout).map_err(EnrollFailure::Pair)
}

/// Write a pairing token owner-only, creating the directory it lives in.
pub(crate) fn write_token(path: &Path, token: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    // Create with the restrictive mode rather than chmod-after-write: a
    // world-readable window, however brief, is a window on a bearer token.
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
        writeln!(file, "{token}")
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, format!("{token}\n"))
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    }
    Ok(())
}

/// How long one probe dial may take: [`PROBE_DEADLINE`], or the test seam.
pub(crate) fn probe_deadline() -> Duration {
    std::env::var(PROBE_DEADLINE_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(PROBE_DEADLINE, Duration::from_millis)
}

/// Dial `target` (`HOST:PORT`) once, briefly, with the given credentials, to
/// learn whether a pinned QUIC route reaches a listener at all.
pub(crate) fn probe(
    target: &str,
    token: &str,
    cert_fingerprint: Option<&str>,
    authority: &phux_client_runtime::target::AuthorityPin,
    identity: Option<TlsClientIdentity>,
) -> Result<(), String> {
    probe_within(
        target,
        token,
        cert_fingerprint,
        authority,
        identity,
        probe_deadline(),
    )
}

/// [`probe`] with its own deadline, for a dial that must not hold an attach
/// up for long.
pub(crate) fn probe_within(
    target: &str,
    token: &str,
    cert_fingerprint: Option<&str>,
    authority: &phux_client_runtime::target::AuthorityPin,
    identity: Option<TlsClientIdentity>,
    deadline: Duration,
) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("could not build a runtime: {err}"))?;
    let plan = super::super::attach::plan_quic_dial(
        &rt,
        target,
        Some(token.to_owned()),
        cert_fingerprint.map(str::to_owned),
        None,
    )
    .and_then(|plan| {
        plan.with_identity(identity)
            .with_authority(target, authority)
    })
    .map_err(|refusal| format!("{refusal:?}"))?;
    rt.block_on(async {
        match tokio::time::timeout(deadline, Connection::connect_dial(&plan.dial)).await {
            Ok(Ok(_connection)) => Ok(()),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err(format!(
                "no answer within {}s; is UDP filtered?",
                deadline.as_secs_f64()
            )),
        }
    })
}

/// The default QUIC port, exposed for the CLI's `default_value_t`.
pub(crate) const fn default_quic_port() -> u16 {
    DEFAULT_QUIC_PORT
}
