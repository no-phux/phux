//! `phux attach --ssh [USER@]HOST` — attach mosh-style over ssh (ADR-0120).
//!
//! ssh does the one thing it is best at, authenticating the operator to a
//! host they already trust, and then gets out of the way. The session rides
//! QUIC, so it roams, renders locally, and gets predictive echo:
//!
//! 1. `ssh -T HOST phux bootstrap` starts the server there if needed and has
//!    it open a listener for this attach alone (`OPEN_LISTENER`);
//! 2. the one JSON line that prints names a port, the certificate
//!    fingerprint to pin, and the token to present. They arrive over the
//!    authenticated ssh channel, so pinning them is not trust on first use;
//! 3. `ssh -G HOST` names the address ssh itself connects to, so an alias in
//!    `~/.ssh/config` dials the machine it names;
//! 4. a short probe dial tells an unreachable UDP port from a slow host, and
//!    the ordinary pinned QUIC attach takes over.
//!
//! Where that cannot work, because UDP is filtered between here and the host
//! or the phux there predates `bootstrap`, the attach falls back to
//! `ssh -t HOST phux attach` and says why. ssh failing outright, or no phux
//! on the host at all, is reported instead: the fallback would fail the same
//! way.

use std::process::{Command, ExitCode, Stdio};

use phux_protocol::PROTOCOL_VERSION;

use super::attach;
use super::enroll::{authority, ssh_hostname, ssh_program};
use super::rec::RecordSpec;

/// ssh's exit status when ssh itself failed (resolution, connection,
/// authentication), as opposed to the remote command.
const SSH_FAILED: i32 = 255;

/// A POSIX shell's "command not found".
const COMMAND_NOT_FOUND: i32 = 127;

/// Everything `phux attach --ssh` was invoked with.
pub(crate) struct SshAttach<'a> {
    /// The ssh destination, passed to ssh verbatim.
    pub(crate) destination: String,
    /// A session to attach to, or `None` for the most recent one.
    pub(crate) session: Option<String>,
    /// The `phux` to run on the host.
    pub(crate) remote_phux: String,
    /// `MIN-MAX` UDP port range for the host's listener, unparsed.
    pub(crate) udp_ports: Option<String>,
    /// Recording spec for the attach that follows.
    pub(crate) rec: Option<&'a RecordSpec>,
}

/// What `phux bootstrap` reported: where to dial and how to authenticate.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Report {
    port: u16,
    cert_fingerprint: String,
    token: String,
    protocol_version: Option<String>,
}

/// Why the bootstrap did not produce a dialable [`Report`].
enum Failure {
    /// ssh itself cannot reach the host, or there is no phux there: the
    /// `ssh -t` fallback would fail the same way, so report and stop.
    Fatal(String),
    /// The host is reachable but cannot bootstrap: fall back to `ssh -t`.
    Fallback(String),
}

/// Run `phux attach --ssh`.
pub(crate) fn run(args: SshAttach<'_>) -> ExitCode {
    if let Err(code) = attach::interactive_tty_preflight() {
        return code;
    }
    if args.destination.is_empty() || args.destination.starts_with('-') {
        eprintln!(
            "phux: --ssh needs an ssh destination such as me@host, got {:?}",
            args.destination
        );
        return ExitCode::from(2);
    }
    if let Some(ports) = args.udp_ports.as_deref()
        && let Err(err) = super::bootstrap::parse_port_range(ports)
    {
        eprintln!("phux: --udp-ports: {err}");
        return ExitCode::from(2);
    }

    eprintln!("phux: bootstrapping {} over ssh…", args.destination);
    let report = match bootstrap(&args) {
        Ok(report) => report,
        Err(Failure::Fatal(message)) => {
            eprintln!("phux: {message}");
            return ExitCode::FAILURE;
        }
        Err(Failure::Fallback(message)) => return fall_back(&args, &message),
    };

    let host = ssh_hostname(&args.destination);
    let target = authority(&host, report.port);
    if let Err(reason) =
        super::enroll::probe(&target, &report.token, Some(&report.cert_fingerprint))
    {
        return fall_back(
            &args,
            &format!("QUIC to {target} did not connect: {reason}"),
        );
    }
    eprintln!("phux: attaching over QUIC to {target}; ssh is out of the path");
    attach::run_attach_quic(
        args.session,
        target,
        Some(report.token),
        Some(report.cert_fingerprint),
        None,
        args.rec,
    )
}

/// Say why the direct path is unavailable, then attach the old way.
fn fall_back(args: &SshAttach<'_>, why: &str) -> ExitCode {
    eprintln!("phux: {why}");
    eprintln!(
        "phux: falling back to `ssh -t {} {} attach` (no roaming or local rendering)",
        args.destination, args.remote_phux
    );
    if args.rec.is_some() {
        eprintln!("phux: --rec does not carry over ssh; run `phux --rec` on the host to record");
    }
    attach::run_attach_over_ssh(
        &args.destination,
        &args.remote_phux,
        args.session.as_deref(),
    )
}

/// Run `phux bootstrap` on the host and read back its report.
///
/// ssh's stderr stays on the terminal, so host-key confirmations, password
/// and 2FA prompts, and the far end's own diagnostics reach the operator as
/// they would in a plain `ssh`. Only stdout is captured.
fn bootstrap(args: &SshAttach<'_>) -> Result<Report, Failure> {
    let program = ssh_program();
    let output = Command::new(&program)
        .arg("-T")
        .arg("-o")
        .arg("ClearAllForwardings=yes")
        .arg("--")
        .arg(&args.destination)
        .arg(remote_command(&args.remote_phux, args.udp_ports.as_deref()))
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|err| {
            Failure::Fatal(format!(
                "could not run {}: {err}",
                program.to_string_lossy()
            ))
        })?;

    match output.status.code() {
        Some(0) => {}
        Some(SSH_FAILED) => {
            return Err(Failure::Fatal(format!(
                "ssh to {} failed (see above)",
                args.destination
            )));
        }
        Some(COMMAND_NOT_FOUND) => {
            return Err(Failure::Fatal(format!(
                "`{}` was not found on {}; install phux there, or name it with --remote-phux PATH",
                args.remote_phux, args.destination
            )));
        }
        code => {
            let status = code.map_or_else(|| "a signal".to_owned(), |code| format!("exit {code}"));
            return Err(Failure::Fallback(format!(
                "`{} bootstrap` failed on {} ({status}); a phux there older than ssh bootstrap \
                 cannot open a listener",
                args.remote_phux, args.destination
            )));
        }
    }

    let report =
        parse_report(&String::from_utf8_lossy(&output.stdout)).map_err(Failure::Fallback)?;
    check_protocol(report.protocol_version.as_deref(), &args.destination)
        .map_err(Failure::Fallback)?;
    Ok(report)
}

/// The command line ssh hands the remote shell. ssh joins its arguments into
/// one string for the far end's shell, so each piece is quoted for it.
fn remote_command(remote_phux: &str, udp_ports: Option<&str>) -> String {
    let mut command = format!(
        "{} bootstrap --client-version {}",
        shell_quote(remote_phux),
        shell_quote(env!("CARGO_PKG_VERSION"))
    );
    if let Some(ports) = udp_ports {
        command.push_str(" --port-range ");
        command.push_str(&shell_quote(ports));
    }
    command
}

/// Quote `word` for a POSIX shell, leaving it bare when that is already
/// safe. A leading `~` stays unquoted so `--remote-phux ~/bin/phux` still
/// expands on the host.
fn shell_quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_-./:@%+=,~".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// Find the listener document in `phux bootstrap`'s stdout.
///
/// Reads the last line that parses as a document with a port, so a shell
/// startup file that prints to stdout on a non-interactive login cannot
/// hide it.
fn parse_report(stdout: &str) -> Result<Report, String> {
    let document = stdout
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .filter(|value| value.get("port").is_some())
        })
        .ok_or_else(|| "`phux bootstrap` printed no listener document".to_owned())?;

    let port = document["port"]
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
        .ok_or_else(|| {
            format!(
                "`phux bootstrap` reported an invalid port: {}",
                document["port"]
            )
        })?;
    let text = |field: &str| {
        document[field]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("`phux bootstrap` reported no {field}"))
    };
    Ok(Report {
        port,
        cert_fingerprint: text("cert_fingerprint")?,
        token: text("token")?,
        protocol_version: document["protocol_version"].as_str().map(str::to_owned),
    })
}

/// Refuse a host whose protocol `major.minor` differs from ours. HELLO
/// would refuse it too, but only after the dial, and with less to go on.
fn check_protocol(theirs: Option<&str>, destination: &str) -> Result<(), String> {
    let Some(theirs) = theirs else {
        return Ok(());
    };
    let mut parts = theirs.split('.').map(str::parse::<u16>);
    let (Some(Ok(major)), Some(Ok(minor))) = (parts.next(), parts.next()) else {
        return Err(format!(
            "{destination} reported an unreadable protocol version {theirs:?}"
        ));
    };
    if (major, minor) == (PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor) {
        return Ok(());
    }
    Err(format!(
        "{destination} speaks phux protocol {theirs}, this phux speaks {}; \
         update one side so both match",
        super::bootstrap::protocol_version()
    ))
}

#[cfg(test)]
mod tests {
    use super::{check_protocol, parse_report, remote_command, shell_quote};

    const DOC: &str = r#"{"schema_version":1,"transport":"quic","port":60123,"cert_fingerprint":"AB:CD","token":"00ff","protocol_version":"0.9.0"}"#;

    #[test]
    fn the_report_is_found_past_startup_noise() {
        let stdout = format!("Welcome to box\nlast login: never\n{DOC}\n\n");
        let report = parse_report(&stdout).expect("report");
        assert_eq!(report.port, 60123);
        assert_eq!(report.cert_fingerprint, "AB:CD");
        assert_eq!(report.token, "00ff");
        assert_eq!(report.protocol_version.as_deref(), Some("0.9.0"));
    }

    #[test]
    fn a_report_missing_what_the_dial_needs_is_refused() {
        assert!(parse_report("").is_err());
        assert!(parse_report("not json\n").is_err());
        assert!(parse_report(r#"{"port":0,"cert_fingerprint":"a","token":"b"}"#).is_err());
        assert!(parse_report(r#"{"port":70000,"cert_fingerprint":"a","token":"b"}"#).is_err());
        assert!(parse_report(r#"{"port":1,"token":"b"}"#).is_err());
        assert!(parse_report(r#"{"port":1,"cert_fingerprint":"a","token":""}"#).is_err());
    }

    #[test]
    fn protocol_major_minor_must_match() {
        let ours = super::super::bootstrap::protocol_version();
        assert!(check_protocol(Some(&ours), "box").is_ok());
        assert!(
            check_protocol(None, "box").is_ok(),
            "an older document says nothing"
        );
        let err = check_protocol(Some("0.1.0"), "box").unwrap_err();
        assert!(err.contains("0.1.0") && err.contains(&ours), "{err}");
        assert!(check_protocol(Some("garbage"), "box").is_err());
    }

    #[test]
    fn the_remote_command_quotes_for_the_far_shell() {
        assert_eq!(shell_quote("phux"), "phux");
        assert_eq!(shell_quote("~/bin/phux"), "~/bin/phux");
        assert_eq!(shell_quote("/opt/my phux"), "'/opt/my phux'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");

        let command = remote_command("phux", Some("60000-61000"));
        assert!(
            command.starts_with("phux bootstrap --client-version "),
            "{command}"
        );
        assert!(command.ends_with(" --port-range 60000-61000"), "{command}");
        assert!(!remote_command("phux", None).contains("--port-range"));
    }
}
