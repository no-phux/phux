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

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_protocol::caps::ServerFeature;
use phux_protocol::wire::frame::{Scope, WHOAMI_KEY, WhoamiRecord};

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::server_target::{ServerSpec, ServerTarget};

/// The correlation id of the verb's one metadata read.
const REQUEST_ID: u32 = 1;

/// Printed in place of an absent field in the prose view.
const NONE: &str = "none";

/// Why the verb has no record to print.
#[derive(Debug)]
enum WhoamiError {
    /// The exchange with the server failed.
    Transport(AttachError),
    /// The server predates the whoami key (no `WHOAMI` feature bit).
    Unsupported,
    /// The server answered, but not with a record.
    BadAnswer(String),
}

impl From<AttachError> for WhoamiError {
    fn from(err: AttachError) -> Self {
        Self::Transport(err)
    }
}

/// A record as the server sent it, plus its typed reading.
///
/// `--json` prints `raw`, so a field a newer server adds reaches the caller
/// even though this binary does not know it.
#[derive(Debug)]
struct Fetched {
    raw: serde_json::Value,
    record: WhoamiRecord,
}

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
        Err(other) => json_err::emit(json, &answer_error(&other), 1),
    }
}

async fn fetch(target: &ServerTarget) -> Result<Fetched, WhoamiError> {
    let mut conn = target.connect().await?;
    fetch_on(&mut conn).await
}

/// Check the feature bit, then read the key. An older server has no such
/// key and would answer "absent", which reads as a server with no idea who
/// its client is; the bit is what makes the refusal honest.
async fn fetch_on(conn: &mut Connection) -> Result<Fetched, WhoamiError> {
    let features = phux_client::state::probe_hello_features(conn).await?;
    if !features.is_some_and(|features| features.contains(ServerFeature::Whoami)) {
        return Err(WhoamiError::Unsupported);
    }
    let (answer, _interleaved) = conn
        .request_metadata(REQUEST_ID, Scope::Global, WHOAMI_KEY.to_owned())
        .await?
        .into_parts();
    let bytes = answer
        .map_err(|refusal| {
            WhoamiError::BadAnswer(format!("the server refused the read: {refusal}"))
        })?
        .ok_or_else(|| {
            WhoamiError::BadAnswer("the server returned no identity for this connection".to_owned())
        })?;
    parse(&bytes)
}

/// Read the value as the documented JSON record.
fn parse(bytes: &[u8]) -> Result<Fetched, WhoamiError> {
    let malformed = |err: serde_json::Error| {
        WhoamiError::BadAnswer(format!("malformed identity record: {err}"))
    };
    let raw: serde_json::Value = serde_json::from_slice(bytes).map_err(malformed)?;
    let record = serde_json::from_value(raw.clone()).map_err(malformed)?;
    Ok(Fetched { raw, record })
}

/// The contract error for a server that answered without a usable record.
fn answer_error(err: &WhoamiError) -> CliError {
    match err {
        WhoamiError::Unsupported => CliError::new(
            codes::SERVER_TOO_OLD,
            "the server does not report connection identity (it predates `phux whoami`)",
            "upgrade phux where the server runs (`phux upgrade` there), then retry; \
             `phux status --json` lists the features a local server advertises",
        ),
        WhoamiError::BadAnswer(detail) => CliError::new(
            codes::TRANSPORT,
            format!("whoami failed: {detail}"),
            "upgrade phux on both ends so they agree on the record, then retry",
        ),
        WhoamiError::Transport(err) => CliError::new(codes::TRANSPORT, err.to_string(), ""),
    }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "tests")]
    #![allow(clippy::unwrap_used, reason = "tests")]

    use phux_client::attach::connection::Connection;
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
    use phux_protocol::wire::frame::{Scope, ServingUser, WHOAMI_KEY, WhoamiRecord};

    use super::{WhoamiError, answer_error, fetch_on, parse, prose_lines};
    use crate::commands::json_err::codes;

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

    /// Serve `spec` on one end of a socket pair and run the verb's exchange
    /// on the other.
    async fn exchange(spec: ScriptSpec) -> Result<super::Fetched, WhoamiError> {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let server_task = tokio::spawn(ScriptedServer::on_stream(server, spec).run());
        let mut conn = Connection::from_stream(client);
        let result = fetch_on(&mut conn).await;
        drop(conn);
        let _ = server_task.await;
        result
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

    /// `--json` prints the server's object as sent, additive fields and all.
    #[test]
    fn json_passes_the_record_through_verbatim() {
        let mut raw = serde_json::to_value(bearer_record()).unwrap();
        raw["later"] = serde_json::json!(true);
        let fetched = parse(raw.to_string().as_bytes()).expect("a record parses");
        assert_eq!(fetched.raw, raw);
        assert_eq!(fetched.record, bearer_record());
    }

    #[test]
    fn a_value_that_is_not_a_record_is_refused() {
        for bytes in [&b"not json"[..], br#"{"schema_version":1}"#] {
            assert!(matches!(parse(bytes), Err(WhoamiError::BadAnswer(_))));
        }
    }

    /// A server without the feature bit is refused before any read.
    #[tokio::test]
    async fn an_older_server_is_refused_with_server_too_old() {
        let result = exchange(ScriptSpec::new()).await;
        let err = result.expect_err("no WHOAMI bit");
        assert!(matches!(err, WhoamiError::Unsupported), "{err:?}");
        let contract = answer_error(&err);
        assert_eq!(contract.code, codes::SERVER_TOO_OLD);
        assert!(contract.message.contains("predates `phux whoami`"));
    }

    /// A current server's record comes back typed and raw.
    #[tokio::test]
    async fn a_current_server_answers_with_the_record() {
        let bytes = serde_json::to_vec(&bearer_record()).unwrap();
        let spec = ScriptSpec::new()
            .server_features(ServerFeatureSet::with(&[ServerFeature::Whoami]))
            .stored_metadata(Scope::Global, WHOAMI_KEY, bytes);
        let fetched = exchange(spec).await.expect("the record is read");
        assert_eq!(fetched.record, bearer_record());
    }

    /// An advertised feature with no value is a broken answer, not "nobody".
    #[tokio::test]
    async fn an_absent_value_is_a_bad_answer() {
        let spec =
            ScriptSpec::new().server_features(ServerFeatureSet::with(&[ServerFeature::Whoami]));
        let err = exchange(spec).await.expect_err("no value");
        assert!(matches!(err, WhoamiError::BadAnswer(_)), "{err:?}");
        assert_eq!(answer_error(&err).code, codes::TRANSPORT);
    }
}
