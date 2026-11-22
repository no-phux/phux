//! Who a connection is to the server it reaches (ADR-0106): one
//! `GET_METADATA` of the read-only `phux.whoami/v1` Global key, which the
//! server answers from the connection's own identity.
//!
//! Shared by `phux whoami` and the `phux_whoami` MCP tool. Each surface
//! dials, then prints the record or the refusal on its own channel.

use phux_protocol::caps::ServerFeature;
use phux_protocol::wire::frame::{Scope, WHOAMI_KEY, WhoamiRecord};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::contract::{Refusal, codes};

/// The correlation id of the verb's one metadata read.
const REQUEST_ID: u32 = 1;

/// Why there is no record to print.
#[derive(Debug)]
pub enum WhoamiError {
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

impl WhoamiError {
    /// The contract refusal for an answer without a usable record. A
    /// [`Self::Transport`] failure is each surface's own no-server report;
    /// this is only its fallback.
    #[must_use]
    pub fn refusal(&self) -> Refusal {
        match self {
            Self::Unsupported => Refusal::new(
                codes::SERVER_TOO_OLD,
                "the server does not report connection identity (it predates `phux whoami`)",
                "upgrade phux where the server runs (`phux upgrade` there), then retry; \
                 `phux status --json` lists the features a local server advertises",
            ),
            Self::BadAnswer(detail) => Refusal::new(
                codes::TRANSPORT,
                format!("whoami failed: {detail}"),
                "upgrade phux on both ends so they agree on the record, then retry",
            ),
            Self::Transport(err) => Refusal::new(codes::TRANSPORT, err.to_string(), ""),
        }
    }
}

/// A record as the server sent it, plus its typed reading.
///
/// `--json` prints `raw`, so a field a newer server adds reaches the caller
/// even though this build does not know it.
#[derive(Debug)]
pub struct Fetched {
    /// The record exactly as the server encoded it.
    pub raw: serde_json::Value,
    /// The fields this build knows.
    pub record: WhoamiRecord,
}

/// Check the feature bit, then read the key. An older server has no such
/// key and would answer "absent", which reads as a server with no idea who
/// its client is; the bit is what makes the refusal honest.
///
/// # Errors
///
/// Any [`WhoamiError`].
pub async fn fetch_on(conn: &mut Connection) -> Result<Fetched, WhoamiError> {
    let features = crate::state::probe_hello_features(conn).await?;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "tests")]
    #![allow(clippy::unwrap_used, reason = "tests")]

    use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
    use phux_protocol::wire::frame::{Scope, ServingUser, WHOAMI_KEY, WhoamiRecord};

    use super::{Fetched, WhoamiError, fetch_on, parse};
    use crate::attach::connection::Connection;
    use crate::contract::codes;
    use crate::testkit::{ScriptSpec, ScriptedServer};

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
    async fn exchange(spec: ScriptSpec) -> Result<Fetched, WhoamiError> {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let server_task = tokio::spawn(ScriptedServer::on_stream(server, spec).run());
        let mut conn = Connection::from_stream(client);
        let result = fetch_on(&mut conn).await;
        drop(conn);
        let _ = server_task.await;
        result
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
        let result = exchange(ScriptSpec::new().server_features(ServerFeatureSet::new())).await;
        let err = result.expect_err("no WHOAMI bit");
        assert!(matches!(err, WhoamiError::Unsupported), "{err:?}");
        let contract = err.refusal();
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
        assert_eq!(err.refusal().code, codes::TRANSPORT);
    }
}
