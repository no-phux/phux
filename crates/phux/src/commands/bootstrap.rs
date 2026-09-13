//! `phux bootstrap` — the far end of `phux attach --ssh` (ADR-0120).
//!
//! `phux attach --ssh HOST` runs this on HOST through ssh. It makes sure the
//! user's server there is running (handing an older build to this one in
//! place, the way naked `phux` does), asks it for a listener that exists for
//! one attach (`OPEN_LISTENER`), and prints one JSON line: the port to dial,
//! the certificate fingerprint to pin, and the token to present. ssh already
//! authenticated whoever is on the other end, and the three values travel
//! back to them over that same channel.
//!
//! Hidden: a human never types it. stdout carries exactly the one JSON line;
//! every diagnostic goes to stderr, which ssh relays to the dialing terminal.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ServerFeature;
use phux_protocol::wire::frame::{
    Command as WireCommand, CommandResult, CommandValue, ListenerTransport,
};
use phux_server::runtime::default_socket_path;

use super::{cli_runtime, command_on};

/// The version of the one-line document `phux bootstrap` prints. A reader
/// tolerates fields it does not know, so adding one does not bump it.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// Everything `phux bootstrap` was invoked with.
pub(crate) struct BootstrapArgs {
    /// The root `--socket`, when given.
    pub(crate) socket: Option<PathBuf>,
    /// The asking client's version, named when it differs from this one.
    pub(crate) client_version: Option<String>,
    /// `MIN-MAX` UDP port range, unparsed.
    pub(crate) port_range: Option<String>,
    /// Requested linger in seconds; `0` is the server default.
    pub(crate) linger: u32,
}

/// Run `phux bootstrap`.
pub(crate) fn run(args: &BootstrapArgs) -> ExitCode {
    let port_range = match args.port_range.as_deref().map(parse_port_range).transpose() {
        Ok(range) => range,
        Err(err) => {
            eprintln!("phux bootstrap: --port-range: {err}");
            return ExitCode::from(2);
        }
    };
    if let Some(theirs) = args.client_version.as_deref()
        && theirs != env!("CARGO_PKG_VERSION")
    {
        eprintln!(
            "phux bootstrap: note: this host runs phux {}, the attaching client is {theirs}",
            env!("CARGO_PKG_VERSION")
        );
    }

    let socket_path = args.socket.clone().unwrap_or_else(default_socket_path);
    if let Err(code) = super::ensure_socket_path_fits(&socket_path) {
        return code;
    }
    if let Err(err) = super::server::ensure_server_for_bootstrap(&socket_path) {
        eprintln!(
            "phux bootstrap: could not start a phux server at {}: {err}",
            socket_path.display()
        );
        return ExitCode::FAILURE;
    }

    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    match rt.block_on(open_listener(&socket_path, port_range, args.linger)) {
        Ok(listener) => {
            outln!("{}", document(listener));
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("phux bootstrap: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Ask the server at `socket_path` for a listener, returning its reply.
async fn open_listener(
    socket_path: &Path,
    port_range: Option<(u16, u16)>,
    linger_secs: u32,
) -> Result<serde_json::Value, String> {
    let mut conn = Connection::connect(socket_path).await.map_err(|err| {
        format!(
            "could not reach the server at {}: {err}",
            socket_path.display()
        )
    })?;
    let supported = conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features
            .contains(ServerFeature::OpenListener)
    });
    if !supported {
        return Err(
            "the running server predates ssh bootstrap; `phux upgrade` on this host hands it \
             to the installed binary without losing sessions"
                .to_owned(),
        );
    }

    let command = WireCommand::OpenListener {
        transport: ListenerTransport::Quic,
        port_range,
        linger_secs,
    };
    match command_on(&mut conn, 1, command).await {
        Ok(CommandResult::OkWith(CommandValue::Json(json))) => serde_json::from_str(&json)
            .map_err(|err| format!("the server's OPEN_LISTENER reply did not parse: {err}")),
        Ok(CommandResult::Error { code, message }) => Err(format!(
            "the server refused OPEN_LISTENER ({code:?}): {message}"
        )),
        Ok(other) => Err(format!("unexpected reply to OPEN_LISTENER: {other:?}")),
        Err(err) => Err(format!("OPEN_LISTENER failed: {err}")),
    }
}

/// The one line `phux bootstrap` prints: the server's listener reply plus
/// the versions the dialing side checks before it dials.
fn document(mut listener: serde_json::Value) -> String {
    if let Some(fields) = listener.as_object_mut() {
        fields.insert("schema_version".to_owned(), SCHEMA_VERSION.into());
        fields.insert(
            "phux_version".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned().into(),
        );
        fields.insert("protocol_version".to_owned(), protocol_version().into());
    }
    listener.to_string()
}

/// This build's protocol version as `major.minor.patch`.
pub(crate) fn protocol_version() -> String {
    format!(
        "{}.{}.{}",
        PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor, PROTOCOL_VERSION.patch
    )
}

/// Parse `MIN-MAX` (or a single `PORT`) into an inclusive, nonzero range.
pub(crate) fn parse_port_range(raw: &str) -> Result<(u16, u16), String> {
    let port = |text: &str| {
        text.trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| format!("{text:?} is not a port in 1-65535"))
    };
    // A single port is the range from it to itself.
    let (min, max) = raw.split_once('-').unwrap_or((raw, raw));
    let (min, max) = (port(min)?, port(max)?);
    if min > max {
        return Err(format!("{raw:?} has its lower bound last"));
    }
    Ok((min, max))
}

#[cfg(test)]
mod tests {
    use super::{document, parse_port_range, protocol_version};

    #[test]
    fn port_ranges_parse_and_refuse_nonsense() {
        assert_eq!(parse_port_range("60000-61000"), Ok((60000, 61000)));
        assert_eq!(parse_port_range("60001"), Ok((60001, 60001)));
        assert_eq!(parse_port_range(" 5 - 9 "), Ok((5, 9)));
        assert!(parse_port_range("9-5").is_err());
        assert!(parse_port_range("0-5").is_err());
        assert!(parse_port_range("1-70000").is_err());
        assert!(parse_port_range("ssh").is_err());
        assert!(parse_port_range("").is_err());
    }

    #[test]
    fn the_document_carries_the_listener_and_both_versions_on_one_line() {
        let line = document(serde_json::json!({
            "schema_version": 1,
            "port": 60123,
            "cert_fingerprint": "AB:CD",
            "token": "00ff",
        }));
        assert!(
            !line.contains('\n'),
            "one line, so the reader can pick it out"
        );
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(parsed["port"], 60123);
        assert_eq!(parsed["token"], "00ff");
        assert_eq!(parsed["phux_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["protocol_version"], protocol_version());
    }
}
