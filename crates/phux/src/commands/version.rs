//! `phux version` — the package and protocol versions of this client and of
//! the server it reaches, the facts a bug report needs in one place.
//!
//! The server's protocol comes from `HELLO_OK`, its package version from
//! `phux whoami`'s record. A server too old for that record reports its
//! protocol only. The target resolves as `phux whoami` resolves it, so
//! `--remote HOST` reports that host's server.

use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::whoami::{WhoamiError, fetch_on};
use phux_protocol::PROTOCOL_VERSION;

use crate::commands::server_target::ServerTarget;

/// What the server reported.
struct Server {
    /// The package version, `None` from a server without `phux.whoami/v1`.
    version: Option<String>,
    protocol: (u16, u16, u16),
}

/// Run `phux version` as parsed. Does not start a server.
pub(crate) fn run(
    args: crate::commands::VersionArgs,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    let json = args.json.json;
    let (rt, target) = match args.remote.with_socket(socket).prepare("version", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    match rt.block_on(fetch(&target)) {
        Ok(server) => {
            if json {
                outln!("{}", document(&server));
            } else {
                for line in prose_lines(Some(&server)) {
                    outln!("{line}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            if !json {
                for line in prose_lines(None) {
                    outln!("{line}");
                }
            }
            target.report_unreachable(json, &err, "version")
        }
    }
}

async fn fetch(target: &ServerTarget) -> Result<Server, AttachError> {
    let mut conn = target.connect().await?;
    let protocol = phux_client::state::probe_hello(&mut conn).await?;
    let version = match fetch_on(&mut conn).await {
        Ok(fetched) => Some(fetched.record.server_version),
        Err(WhoamiError::Transport(err)) => return Err(err),
        Err(_) => None,
    };
    Ok(Server { version, protocol })
}

fn triple((major, minor, patch): (u16, u16, u16)) -> String {
    format!("{major}.{minor}.{patch}")
}

const fn client_protocol() -> (u16, u16, u16) {
    (
        PROTOCOL_VERSION.major,
        PROTOCOL_VERSION.minor,
        PROTOCOL_VERSION.patch,
    )
}

/// One labelled field per line, the JSON names as labels (as `phux whoami`
/// prints). Without a server, the client's lines only. Pure, for tests.
fn prose_lines(server: Option<&Server>) -> Vec<String> {
    let mut fields = vec![
        ("client", env!("PHUX_VERSION_LABEL").to_owned()),
        ("client_protocol", triple(client_protocol())),
    ];
    if let Some(server) = server {
        let version = server.version.as_deref().unwrap_or("unknown");
        fields.push(("server", version.to_owned()));
        fields.push(("server_protocol", triple(server.protocol)));
    }
    fields
        .into_iter()
        .map(|(label, value)| format!("{:<17}{value}", format!("{label}:")))
        .collect()
}

fn protocol_json((major, minor, patch): (u16, u16, u16)) -> serde_json::Value {
    serde_json::json!({ "major": major, "minor": minor, "patch": patch })
}

fn document(server: &Server) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "client": {
            "version": env!("PHUX_VERSION_LABEL"),
            "protocol": protocol_json(client_protocol()),
        },
        "server": {
            "version": server.version,
            "protocol": protocol_json(server.protocol),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{Server, document, prose_lines};

    #[test]
    fn prose_names_both_sides_and_marks_an_unknown_server_version() {
        let server = Server {
            version: None,
            protocol: (1, 2, 3),
        };
        let lines = prose_lines(Some(&server));
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("client:          "));
        assert_eq!(lines[2], "server:          unknown");
        assert_eq!(lines[3], "server_protocol: 1.2.3");
        assert_eq!(prose_lines(None).len(), 2, "no server, client lines only");

        let doc = document(&server);
        assert!(doc["server"]["version"].is_null());
        assert_eq!(doc["server"]["protocol"]["minor"], 2);
    }
}
