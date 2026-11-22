//! `phux whoami` — who this connection is to the server it reaches
//! (ADR-0106).
//!
//! One `GET_METADATA` of the read-only `phux.whoami/v1` Global key, which the
//! server answers from the connection's own identity: the principal and
//! credential id of a bearer connection, the kernel peer uid of a Unix-socket
//! one, the auth route, and the OS user and host the server runs as. The verb
//! reads and prints. It never changes identity: a phux server never switches
//! users, so the serving user is also the user every pane runs as.
//!
//! The target is the local socket or a `--remote` host, resolved exactly as
//! `phux ls` resolves it (`server_target`), so `phux whoami --remote me@mini`
//! reports what that dial authenticated as there.

use std::process::ExitCode;

use phux_client::whoami::{Fetched, WhoamiError, fetch_on};
use phux_protocol::wire::frame::WhoamiRecord;

use crate::commands::json_err;
use crate::commands::server_target::{ServerSpec, ServerTarget};

/// Printed in place of an absent field in the prose view.
const NONE: &str = "none";

/// `phux whoami [--remote HOST] [--json]`. Does not start a server.
pub(crate) fn run_whoami(json: bool, server: ServerSpec) -> ExitCode {
    let (rt, target) = match server.prepare("whoami", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    match rt.block_on(fetch(&target)) {
        Ok(fetched) => {
            print_record(json, &fetched);
            ExitCode::SUCCESS
        }
        Err(WhoamiError::Transport(err)) => target.report_unreachable(json, &err, "whoami"),
        Err(other) => json_err::emit(json, &other.refusal(), 1),
    }
}

async fn fetch(target: &ServerTarget) -> Result<Fetched, WhoamiError> {
    let mut conn = target.connect().await?;
    fetch_on(&mut conn).await
}

fn print_record(json: bool, fetched: &Fetched) {
    if json {
        outln!("{}", fetched.raw);
        return;
    }
    for line in prose_lines(&fetched.record) {
        outln!("{line}");
    }
}

/// The prose view: one field per line, labelled with the JSON field names so
/// a reader can move between the two views without a translation table.
/// Pure, for tests.
fn prose_lines(record: &WhoamiRecord) -> Vec<String> {
    let serving_user = record.serving_user.name.as_ref().map_or_else(
        || format!("uid {}", record.serving_user.uid),
        |name| format!("{name} (uid {})", record.serving_user.uid),
    );
    let fields = [
        ("principal", or_none(record.principal.as_deref())),
        ("credential_id", or_none(record.credential_id.as_deref())),
        ("auth_route", record.auth_route.clone()),
        (
            "ssh_client",
            record.ssh_client.as_ref().map_or_else(
                || NONE.to_owned(),
                |client| format!("{} port {}", client.addr, client.port),
            ),
        ),
        (
            "peer_uid",
            record
                .peer_uid
                .map_or_else(|| NONE.to_owned(), |uid| uid.to_string()),
        ),
        ("serving_user", serving_user),
        ("host", record.host.clone()),
        ("server_version", record.server_version.clone()),
    ];
    fields
        .into_iter()
        .map(|(label, value)| format!("{:<16}{value}", format!("{label}:")))
        .collect()
}

fn or_none(value: Option<&str>) -> String {
    value.unwrap_or(NONE).to_owned()
}

/// Run `phux whoami` as parsed.
pub(crate) fn run(
    args: crate::commands::WhoamiArgs,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    run_whoami(args.json.json, args.remote.with_socket(socket))
}

#[cfg(test)]
mod tests {
    use phux_protocol::wire::frame::{ServingUser, WhoamiRecord};

    use super::prose_lines;

    fn bearer_record() -> WhoamiRecord {
        WhoamiRecord {
            schema_version: 1,
            principal: Some("phone".to_owned()),
            credential_id: Some("0123abcd".to_owned()),
            auth_route: "bearer-quic".to_owned(),
            peer_uid: None,
            serving_user: ServingUser {
                uid: 501,
                name: Some("me".to_owned()),
            },
            host: "mini".to_owned(),
            server_version: "0.30.0".to_owned(),
            ssh_client: None,
        }
    }

    #[test]
    fn prose_prints_one_labelled_field_per_line() {
        assert_eq!(
            prose_lines(&bearer_record()),
            [
                "principal:      phone",
                "credential_id:  0123abcd",
                "auth_route:     bearer-quic",
                "ssh_client:     none",
                "peer_uid:       none",
                "serving_user:   me (uid 501)",
                "host:           mini",
                "server_version: 0.30.0",
            ]
        );
    }

    #[test]
    fn prose_names_the_ssh_client_of_an_ssh_stdio_route() {
        let mut record = bearer_record();
        record.auth_route = "ssh-stdio".to_owned();
        record.ssh_client = Some(phux_protocol::wire::frame::SshClient {
            addr: "2001:db8::1".to_owned(),
            port: 40000,
        });
        assert_eq!(
            prose_lines(&record)[3],
            "ssh_client:     2001:db8::1 port 40000"
        );
    }

    #[test]
    fn prose_names_absent_fields_and_a_nameless_uid() {
        let mut record = bearer_record();
        record.principal = None;
        record.credential_id = None;
        record.auth_route = "uds".to_owned();
        record.peer_uid = Some(501);
        record.serving_user.name = None;
        let lines = prose_lines(&record);
        assert_eq!(lines[0], "principal:      none");
        assert_eq!(lines[4], "peer_uid:       501");
        assert_eq!(lines[5], "serving_user:   uid 501");
    }
}
