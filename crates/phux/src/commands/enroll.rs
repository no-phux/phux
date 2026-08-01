//! `phux enroll HOST` — bootstrap a remote server's credentials over ssh
//! (ADR-0055).
//!
//! The remote path already worked and was still unusable: `phux pair` prints
//! a 64-hex token and a 64-hex fingerprint, and the operator retypes both
//! into a command line they retype again on every attach. The unexploited
//! asset is ssh. Anyone self-hosting a phux server already holds ssh trust to
//! that host — they used it to install phux — and that channel is
//! authenticated, confidential, and already inside their threat model.
//!
//! So enrollment grants no authority ssh did not already grant: whoever can
//! `ssh HOST` can run `phux pair` there and read the token themselves. What
//! it removes is a transcription error class, and the reason nobody used the
//! remote path.
//!
//! The flow, all over one ssh channel:
//!
//! 1. confirm `phux` is on the remote `PATH`;
//! 2. install the remote's service unit, so the server survives reboot;
//! 3. run `phux pair --json` there and read back token + fingerprint;
//! 4. pick a dialable endpoint from the remote's detected overlay address;
//! 5. write the token locally 0o600 and register the `[[remote]]` entry.
//!
//! Step 4 is the one that can fail benignly: a host with no overlay address
//! and no configured listener has nothing to dial. That is not an error — it
//! falls back to an `ssh://` entry, which still gives the operator
//! `phux attach HOST` against a server whose sessions outlive the connection.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::remote::{self, NewRemote};

/// Default QUIC port for an enrolled server. Matches the port
/// `docs/remote-access.md` uses throughout.
const DEFAULT_QUIC_PORT: u16 = 8788;

/// What `phux pair --json` reported on the remote host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairReport {
    pub(crate) token: String,
    pub(crate) cert_fingerprint: Option<String>,
    pub(crate) overlay_addresses: Vec<String>,
}

impl PairReport {
    /// Parse the document `phux pair --json` writes.
    ///
    /// Tolerant of fields it does not know so a newer remote phux can add
    /// keys without breaking an older `phux enroll`; strict about the two it
    /// cannot proceed without.
    pub(crate) fn parse(stdout: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(stdout.trim())
            .map_err(|err| format!("remote `phux pair --json` emitted unparseable JSON: {err}"))?;

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

        Ok(Self {
            token,
            cert_fingerprint,
            overlay_addresses,
        })
    }
}

/// Choose the endpoint to register for an enrolled host.
///
/// Prefers a real transport, and degrades honestly rather than registering
/// something that cannot be dialed:
///
/// * an operator-supplied `--host` always wins — they know their network;
/// * otherwise a detected overlay address plus the QUIC port, but only when
///   the remote also produced a certificate fingerprint, since ADR-0031
///   refuses an unpinned routable dial and registering one would just move
///   the failure later;
/// * otherwise `ssh://HOST`, which needs no pairing at all.
pub(crate) fn choose_endpoint(
    ssh_host: &str,
    report: &PairReport,
    host_override: Option<&str>,
    quic_port: u16,
) -> String {
    if let Some(host) = host_override {
        return if host.contains("://") {
            host.to_owned()
        } else {
            format!("quic://{host}")
        };
    }
    if report.cert_fingerprint.is_some()
        && let Some(addr) = report.overlay_addresses.first()
    {
        let host = if addr.contains(':') {
            // An IPv6 literal needs brackets to stay parseable as HOST:PORT.
            format!("[{addr}]")
        } else {
            addr.clone()
        };
        return format!("quic://{host}:{quic_port}");
    }
    format!("ssh://{ssh_host}")
}

/// The local path a remote's token is written to.
///
/// Under the state dir beside the rest of phux's credential material, named
/// for the remote so two enrollments never collide.
pub(crate) fn token_path(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join("remotes").join(format!("{name}.token"))
}

/// The local label for an enrolled host: the ssh destination with any
/// `user@` and trailing path stripped, so `phux enroll me@mini` registers
/// `mini`.
pub(crate) fn default_name(ssh_host: &str) -> String {
    ssh_host
        .rsplit('@')
        .next()
        .unwrap_or(ssh_host)
        .split(['/', ':'])
        .next()
        .unwrap_or(ssh_host)
        .to_owned()
}

/// `phux enroll HOST`.
#[allow(
    clippy::too_many_arguments,
    reason = "CLI entry point; every argument is one documented flag"
)]
pub(crate) fn run_enroll(
    ssh_host: &str,
    name: Option<&str>,
    host_override: Option<&str>,
    quic_port: u16,
    install_service: bool,
    ssh_only: bool,
    session: Option<&str>,
) -> ExitCode {
    let name = name.map_or_else(|| default_name(ssh_host), str::to_owned);

    // `--ssh-only` skips the remote entirely: no pairing, no service, just a
    // registry entry. Useful when the operator does not want a listener at
    // all, and as an escape hatch when pairing misbehaves.
    if ssh_only {
        let endpoint = format!("ssh://{ssh_host}");
        let new = match NewRemote::new(&name, &endpoint, None, None, session) {
            Ok(new) => new,
            Err(err) => {
                eprintln!("phux enroll: {err}");
                return ExitCode::FAILURE;
            }
        };
        return finish(&new, &name, &endpoint);
    }

    outln!("Enrolling {ssh_host}...");

    if let Err(err) = remote_phux_version(ssh_host) {
        eprintln!("phux enroll: {err}");
        eprintln!(
            "      Install phux on {ssh_host} first, or run \
             `phux enroll {ssh_host} --ssh-only` to register it anyway."
        );
        return ExitCode::FAILURE;
    }

    if install_service {
        let quic_bind = format!("0.0.0.0:{quic_port}");
        match ssh_capture(
            ssh_host,
            &["phux", "service", "install", "--quic", &quic_bind],
        ) {
            Ok(_) => outln!("  service installed on {ssh_host} (quic {quic_bind})"),
            Err(err) => {
                // Not fatal: an operator may supervise the server another
                // way, and pairing is still worth doing.
                eprintln!("phux enroll: warning: could not install the remote service: {err}");
            }
        }
    }

    // `phux pair` writes the token store the server reads at startup, so it
    // must run after the service install — which is also what makes
    // PHUX_QUIC_ADDR visible to the pair invocation's overlay-derived link.
    let stdout = match ssh_capture(ssh_host, &["phux", "pair", "--json"]) {
        Ok(stdout) => stdout,
        Err(err) => {
            eprintln!("phux enroll: {err}");
            return ExitCode::FAILURE;
        }
    };

    let report = match PairReport::parse(&stdout) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("phux enroll: {err}");
            return ExitCode::FAILURE;
        }
    };

    let endpoint = choose_endpoint(ssh_host, &report, host_override, quic_port);

    // The token is only meaningful for a dialed transport; an ssh:// entry
    // rides ssh trust and must not leave a stray credential on disk.
    let token_file = if endpoint.starts_with("ssh://") {
        None
    } else {
        Some(token_path(&phux_server::telemetry::state_dir(), &name))
    };

    // Validate the whole registry entry BEFORE the token hits disk: a
    // rejected name (`../x` would escape <state>/remotes via token_path's
    // join) or a quic endpoint missing its fingerprint must not leave an
    // orphaned bearer token behind. `validate_token_file` accepts a
    // not-yet-written path by design.
    let new = match NewRemote::new(
        &name,
        &endpoint,
        token_file.as_deref(),
        report.cert_fingerprint.as_deref(),
        session,
    ) {
        Ok(new) => new,
        Err(err) => {
            eprintln!("phux enroll: {err}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(path) = &token_file
        && let Err(err) = write_token(path, &report.token)
    {
        eprintln!("phux enroll: {err}");
        return ExitCode::FAILURE;
    }

    finish(&new, &name, &endpoint)
}

/// Register the (already validated) entry and report what an operator can
/// now type.
fn finish(new: &NewRemote, name: &str, endpoint: &str) -> ExitCode {
    if let Err(err) = remote::add_or_update(new) {
        eprintln!("phux enroll: {err}");
        return ExitCode::FAILURE;
    }

    outln!();
    outln!("Enrolled {name} -> {endpoint}");
    if endpoint.starts_with("ssh://") {
        outln!(
            "  No listener to dial, so this rides ssh. Sessions still live on the\n  \
             server and survive the connection dropping. Add `--quic` on the\n  \
             remote's service and re-run enroll to upgrade the transport."
        );
    } else {
        outln!("  certificate pinned, token stored 0600");
    }
    outln!();
    outln!("  phux attach {name}");
    ExitCode::SUCCESS
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

/// Confirm `phux` is on the remote `PATH` before doing anything that assumes
/// it, so the failure names the real problem instead of surfacing as a
/// baffling empty pair document.
pub(crate) fn remote_phux_version(ssh_host: &str) -> Result<String, String> {
    ssh_capture(ssh_host, &["phux", "--version"])
}

/// Run a command on the remote host over ssh and capture its stdout.
///
/// Honors `$PHUX_SSH`, the same seam the federation hub's satellite dialer
/// uses, so a custom ssh wrapper works for both. `BatchMode=yes` turns a
/// missing key into a prompt-free error rather than a hung enrollment
/// waiting on a password nobody is watching for.
pub(crate) fn ssh_capture(ssh_host: &str, argv: &[&str]) -> Result<String, String> {
    let program = std::env::var_os("PHUX_SSH").unwrap_or_else(|| "ssh".into());
    let output = std::process::Command::new(&program)
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(ssh_host)
        .args(argv)
        .output()
        .map_err(|err| format!("could not run {}: {err}", program.to_string_lossy()))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    let detail = if detail.is_empty() {
        format!("exit {}", output.status)
    } else {
        detail.to_owned()
    };
    Err(format!(
        "`ssh {ssh_host} {}` failed: {detail}",
        argv.join(" ")
    ))
}

/// The default QUIC port, exposed for the CLI's `default_value_t`.
pub(crate) const fn default_quic_port() -> u16 {
    DEFAULT_QUIC_PORT
}

#[cfg(test)]
mod tests {
    use super::{PairReport, choose_endpoint, default_name, token_path, write_token};
    use std::path::Path;

    fn report(fp: Option<&str>, overlay: &[&str]) -> PairReport {
        PairReport {
            token: "deadbeef".to_owned(),
            cert_fingerprint: fp.map(str::to_owned),
            overlay_addresses: overlay.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[test]
    fn parses_the_pair_document() {
        let parsed = PairReport::parse(
            r#"{
              "token": "abc123",
              "cert_fingerprint": "AB:CD",
              "overlay_addresses": ["100.64.0.2"],
              "ws_addr": null,
              "quic_addr": "0.0.0.0:8788"
            }"#,
        )
        .expect("parse");
        assert_eq!(parsed.token, "abc123");
        assert_eq!(parsed.cert_fingerprint.as_deref(), Some("AB:CD"));
        assert_eq!(parsed.overlay_addresses, vec!["100.64.0.2".to_owned()]);
    }

    #[test]
    fn tolerates_unknown_fields_from_a_newer_remote() {
        // A newer remote phux must be able to add keys without breaking an
        // older `phux enroll`.
        let parsed = PairReport::parse(r#"{"token":"t","brand_new_field":42}"#).expect("parse");
        assert_eq!(parsed.token, "t");
        assert_eq!(parsed.cert_fingerprint, None);
        assert!(parsed.overlay_addresses.is_empty());
    }

    #[test]
    fn refuses_a_document_with_no_token() {
        assert!(PairReport::parse(r#"{"cert_fingerprint":"AB"}"#).is_err());
        assert!(PairReport::parse(r#"{"token":""}"#).is_err());
        // Not JSON at all — the shape of an ssh banner leaking into stdout.
        assert!(PairReport::parse("Welcome to Ubuntu\n").is_err());
    }

    #[test]
    fn prefers_quic_on_the_overlay_address_when_pinned() {
        assert_eq!(
            choose_endpoint("mini", &report(Some("AB"), &["100.64.0.2"]), None, 8788),
            "quic://100.64.0.2:8788"
        );
    }

    #[test]
    fn falls_back_to_ssh_when_there_is_nothing_dialable() {
        // No overlay address: nothing to dial.
        assert_eq!(
            choose_endpoint("mini", &report(Some("AB"), &[]), None, 8788),
            "ssh://mini"
        );
        // Overlay address but no pin: ADR-0031 would refuse the dial, so
        // registering it would only move the failure later.
        assert_eq!(
            choose_endpoint("mini", &report(None, &["100.64.0.2"]), None, 8788),
            "ssh://mini"
        );
    }

    #[test]
    fn operator_host_override_always_wins() {
        let report = report(Some("AB"), &["100.64.0.2"]);
        assert_eq!(
            choose_endpoint("mini", &report, Some("mini.ts.net:9999"), 8788),
            "quic://mini.ts.net:9999"
        );
        // A full URI passes through, so wss:// is reachable this way.
        assert_eq!(
            choose_endpoint("mini", &report, Some("wss://mini.ts.net:8787"), 8788),
            "wss://mini.ts.net:8787"
        );
    }

    #[test]
    fn ipv6_overlay_address_is_bracketed() {
        // Without brackets the result would not parse as HOST:PORT.
        assert_eq!(
            choose_endpoint("mini", &report(Some("AB"), &["fd7a::1"]), None, 8788),
            "quic://[fd7a::1]:8788"
        );
    }

    #[test]
    fn default_name_strips_user_and_path() {
        assert_eq!(default_name("mini"), "mini");
        assert_eq!(default_name("phall@mini"), "mini");
        assert_eq!(default_name("phall@mini.ts.net"), "mini.ts.net");
    }

    #[test]
    fn token_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = token_path(dir.path(), "mini");
        write_token(&path, "deadbeef").expect("write");

        assert_eq!(std::fs::read_to_string(&path).expect("read"), "deadbeef\n");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a bearer token must not be readable");
    }

    #[test]
    fn token_path_is_namespaced_per_remote() {
        let state = Path::new("/state");
        assert_eq!(
            token_path(state, "mini"),
            Path::new("/state/remotes/mini.token")
        );
        assert_ne!(token_path(state, "mini"), token_path(state, "studio"));
    }
}
