//! The read-only diagnostic tools `phux_status`, `phux_doctor`, and
//! `phux_whoami`: separate tools (ADR-0071 point 7(b)), each the canonical
//! CLI's versioned document, so there is one implementation of every check.
//! `status` and `doctor` run the CLI; `whoami` calls the
//! `phux_client::whoami` read the CLI verb calls.
//!
//! `status` and `doctor` spend exit 1 on their interesting answer (a stopped
//! server, a failed check) and still print the document, so exit 1 is
//! allowed; an empty stdout under exit 1 is the real failure and reports the
//! CLI's stderr error line. `whoami` has no such answer: every non-zero exit
//! is its stderr error contract. There is deliberately no repair tool and no
//! log reader.

#![allow(
    clippy::similar_names,
    reason = "argv and parsed args are deliberately adjacent in thin CLI wrappers"
)]

use std::path::Path;

use phux_client::attach::connection::Connection;
use phux_client::whoami::{Fetched, WhoamiError, fetch_on};
use serde_json::{Value, json};

use crate::cli_adapter::{CliAdapter, DEFAULT_CALL_TIMEOUT, push_socket};
use crate::cli_tools::schema;
#[cfg(test)]
use crate::tools::call_with_adapter;
use crate::tools::{
    ToolError, ToolSpec, cli_refusal_error, socket_arg, strict_object, transport_refusal_at,
};

/// The exit `status` and `doctor` spend on a result (see the module docs).
const EXIT_ANSWERED: i32 = 1;

/// Shared `socket` description.
const SOCKET_DESC: &str = "Override the UDS path of the server to diagnose. \
    Defaults to PHUX_SOCKET or the daemon default.";

/// The diagnostic tools, in catalog order.
pub(crate) const SPECS: &[ToolSpec] = &[
    ToolSpec::cli("phux_status", status_schema, |args, adapter| {
        Box::pin(run_diagnostic("status", args, adapter))
    }),
    ToolSpec::cli("phux_doctor", doctor_schema, |args, adapter| {
        Box::pin(run_diagnostic("doctor", args, adapter))
    }),
    ToolSpec::in_process("phux_whoami", whoami_schema, |args| {
        Box::pin(run_whoami(args))
    }),
];

fn status_schema() -> Value {
    schema(
        "phux_status",
        "Report the server behind one socket: whether it is running, its pid, when it bound the \
         socket, the negotiated protocol version, attached-client and session counts, satellite \
         panes, unreachable satellites, and the log paths to read next. READ-ONLY, and it never \
         auto-starts a server — asking whether a server is running may not create one. \
         A STOPPED SERVER IS AN ANSWER, NOT AN ERROR: the CLI exits non-zero and this tool still \
         returns the document, so branch on `running`, never on whether the call succeeded. When \
         `running` is false the document also carries `error` {code, message} and `remedy` from \
         the same closed vocabulary every other phux JSON verb uses. \
         `pid` MAY BE NULL ON A RUNNING SERVER — it comes from the socket's peer credentials \
         rather than from the server itself — so never read a null pid as \"no server\"; \
         `running` is the only field that answers that. `unreachable` is always present and \
         empty means the fleet view is complete. \
         This describes one server at one socket. Use phux_doctor for the wider question of \
         whether the install is healthy: crash-looping, version-skewed, or supervised by a \
         legacy unit.",
        socket_properties(),
        &[],
    )
}

fn doctor_schema() -> Value {
    schema(
        "phux_doctor",
        "Run every phux health check and return the whole verdict: config parse, instance/profile \
         isolation, socket path length, server reachability, server-health, plugin manifests, the \
         agent shim, and log paths. This is the same code path as `phux doctor`, so the two can \
         never disagree about whether the install is healthy. READ-ONLY — a diagnostic that \
         repairs things is a diagnostic nobody can trust — and it starts nothing. \
         A FAILING CHECK IS AN ANSWER, NOT AN ERROR: the CLI exits non-zero when any check failed \
         and this tool still returns the document, so branch on `ok` and on each check's \
         `status`, never on whether the call succeeded. \
         Result: `{schema_version, ok, failed, checks: [{name, status, detail, hint}]}` with \
         `status` one of `pass`, `warn`, `fail`. \
         READ EVERY ROW, NOT THE FIRST: check names are NOT unique. `server-health` reports one \
         row per condition that holds — a crash-loop (the server restarted repeatedly inside the \
         start-history window), a legacy supervisor unit that restarts on every exit unthrottled, \
         and version skew between the running server and this binary — and those co-occur more \
         than they don't, because a legacy unit is exactly what turns a dying server into a \
         crash-loop. \
         `warn` means a check could not be verified or does not apply right now, and is \
         deliberately NOT a pass: a stopped server warns rather than failing, because that is a \
         normal state and not a broken install. Every `warn` and `fail` carries a `hint` naming \
         the remedy; relay it, do not run it — the remedies restart supervised services and \
         rewrite units on the human's machine. \
         Only the socket and server checks follow `socket`; everything else describes the machine \
         this adapter is running on.",
        socket_properties(),
        &[],
    )
}

fn whoami_schema() -> Value {
    schema(
        "phux_whoami",
        "Report who this connection is to the server behind one socket, as that server sees it: \
         the credential `principal` and non-secret `credential_id` (null on the local socket), \
         the `auth_route` (an open vocabulary such as `uds`, `ssh-stdio`, or `bearer-quic`; show \
         an unknown value as-is), the kernel `peer_uid` (local socket only), the `serving_user` \
         {uid, name} the server and every pane run as, the `host`, the `server_version`, and on \
         an `ssh-stdio` route the `ssh_client` {addr, port} the stdio-bridge reported (null \
         otherwise; a report, not an authenticated fact). This is the same code path as \
         `phux whoami --json` and returns its document unchanged: `{schema_version: 1, \
         principal, credential_id, auth_route, peer_uid, serving_user, host, server_version, \
         ssh_client}`; ignore fields you do not know. READ-ONLY and idempotent: it reads one \
         server-owned key, never changes identity, and never auto-starts a server. \
         A SERVER THAT PREDATES THE KEY IS REFUSED, NOT GUESSED: without the `whoami` feature \
         the call fails with `server_too_old` rather than returning an empty identity.",
        socket_properties(),
        &[],
    )
}

fn socket_properties() -> Value {
    json!({ "socket": { "type": "string", "minLength": 1, "maxLength": 4096, "description": SOCKET_DESC } })
}

/// `phux whoami --json`'s record, or the error line that verb prints.
async fn run_whoami(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["socket"], &[])?;
    let socket = socket_arg(args)?;
    let fetched = fetch_whoami(&socket).await.map_err(|err| {
        let refusal = match &err {
            WhoamiError::Transport(err) => transport_refusal_at(err, &socket, "whoami"),
            other => other.refusal(),
        };
        cli_refusal_error(&refusal, 1)
    })?;
    Ok(fetched.raw)
}

async fn fetch_whoami(socket: &Path) -> Result<Fetched, WhoamiError> {
    let mut conn = Connection::connect(socket).await?;
    fetch_on(&mut conn).await
}

/// Execute `phux <verb> --json` and return its document.
async fn run_diagnostic(
    verb: &str,
    args: &Value,
    adapter: &CliAdapter,
) -> Result<Value, ToolError> {
    strict_object(args, &["socket"], &[])?;
    let mut argv = vec![verb.to_owned(), "--json".to_owned()];
    push_socket(&mut argv, args)?;
    let output = adapter
        .run_allowing(argv, DEFAULT_CALL_TIMEOUT, &[EXIT_ANSWERED])
        .await?;

    // Exit 1 with an empty stdout is a real failure, not a document.
    if output.stdout.trim().is_empty() {
        let message = output.stderr.trim();
        return Err(ToolError::new(if message.is_empty() {
            format!("phux {verb} --json exited without printing a document")
        } else {
            message.to_owned()
        }));
    }
    serde_json::from_str(&output.stdout).map_err(|err| {
        ToolError::new(format!(
            "phux {verb} --json returned malformed JSON: {err}; stdout={:?}",
            output.stdout
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phux_protocol::wire::frame::{ServingUser, WhoamiRecord};
    use tempfile::TempDir;

    use super::*;
    use crate::cli_adapter::fake;

    /// A fake `phux` answering each diagnostic verb on its interesting
    /// path: the whole document on stdout under exit 1.
    fn fake_cli() -> (TempDir, CliAdapter, PathBuf) {
        fake::cli(
            r#"case "$1" in
  status) printf '{"schema_version":1,"running":false,"error":{"code":"no_server"}}\n'
          exit 1 ;;
  doctor) printf '{"schema_version":1,"ok":false,"failed":1,"checks":[{"name":"server-health","status":"fail"}]}\n'
          exit 1 ;;
esac
"#,
        )
    }

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

    /// `phux_whoami` returns the server's record unchanged: the shared
    /// protocol record, plus any field a newer server adds.
    #[tokio::test]
    async fn whoami_returns_the_record_verbatim() {
        use phux_client::testkit::{self, ScriptSpec};
        use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
        use phux_protocol::wire::frame::{Scope, WHOAMI_KEY};

        let mut raw = serde_json::to_value(bearer_record()).unwrap();
        raw["later"] = json!(true);
        let bytes = raw.to_string().into_bytes();
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("whoami.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(testkit::serve_every(listener, move || {
            ScriptSpec::new()
                .server_features(ServerFeatureSet::with(&[ServerFeature::Whoami]))
                .stored_metadata(Scope::Global, WHOAMI_KEY, bytes.clone())
        }));
        let result = run_whoami(&json!({ "socket": socket.to_string_lossy() })).await;
        server.abort();
        let result = result.expect("the record is read");
        assert_eq!(result, raw, "the record must pass through unchanged");
        let record: WhoamiRecord = serde_json::from_value(result).unwrap();
        assert_eq!(record, bearer_record());
    }

    /// The descriptions carry the rules that make results readable: a
    /// non-zero exit is the answer, and doctor's `server-health` repeats.
    #[test]
    fn the_descriptions_state_the_answer_not_error_rule() {
        let status = status_schema();
        let status = status["description"].as_str().unwrap();
        assert!(
            status.contains("A STOPPED SERVER IS AN ANSWER, NOT AN ERROR"),
            "{status}",
        );
        assert!(status.contains("branch on `running`"), "{status}");
        assert!(
            status.contains("MAY BE NULL ON A RUNNING SERVER"),
            "a null pid must not read as \"no server\": {status}",
        );
        assert!(status.contains("never auto-starts"), "{status}");

        let doctor = doctor_schema();
        let doctor = doctor["description"].as_str().unwrap();
        assert!(
            doctor.contains("A FAILING CHECK IS AN ANSWER, NOT AN ERROR"),
            "{doctor}",
        );
        assert!(
            doctor.contains("READ EVERY ROW, NOT THE FIRST"),
            "the co-occurring server-health rows must be stated: {doctor}",
        );
        assert!(doctor.contains("READ-ONLY"), "{doctor}");
        assert!(
            doctor.contains("relay it, do not run it"),
            "the hints name remedies that restart services: {doctor}",
        );

        let whoami = whoami_schema();
        let whoami = whoami["description"].as_str().unwrap();
        assert!(whoami.contains("READ-ONLY and idempotent"), "{whoami}");
        assert!(whoami.contains("never auto-starts"), "{whoami}");
        assert!(
            whoami.contains("`server_too_old`"),
            "the older-server refusal must be stated: {whoami}",
        );
    }

    /// Each tool executes the exact canonical argv, and the interesting
    /// non-zero exit returns the document instead of an error.
    #[tokio::test]
    async fn each_tool_executes_canonical_argv_and_keeps_the_document() {
        let (_temp, adapter, log) = fake_cli();

        let status = fake::assert_argv(
            &adapter,
            &log,
            "phux_status",
            json!({ "socket": "/sock" }),
            &["status", "--json", "--socket", "/sock"],
        )
        .await;
        assert_eq!(
            status["running"],
            json!(false),
            "exit 1 must arrive as `running: false`, not as a tool error",
        );
        assert_eq!(status["error"]["code"], json!("no_server"));

        let doctor = fake::assert_argv(
            &adapter,
            &log,
            "phux_doctor",
            json!({}),
            &["doctor", "--json"],
        )
        .await;
        assert_eq!(
            doctor["ok"],
            json!(false),
            "exit 1 must arrive as `ok: false`, not as a tool error",
        );
        assert_eq!(doctor["checks"][0]["name"], json!("server-health"));
    }

    /// The other exit 1: no document, one JSON error object on stderr. The
    /// caller gets that contract line, not a malformed-JSON complaint.
    #[tokio::test]
    async fn a_document_less_failure_reports_the_cli_error_contract() {
        let (_temp, adapter, _log) = fake::scripted(
            "",
            "{\"schema_version\":1,\"error\":{\"code\":\"server_disconnected\"}}\n",
            1,
        );
        for name in ["phux_status", "phux_doctor"] {
            let err = call_with_adapter(name, &json!({}), &adapter)
                .await
                .expect_err("an empty stdout is a failure, not a document");
            assert!(
                err.0.contains("server_disconnected"),
                "{name} lost the CLI's error contract: {err:?}",
            );
            assert!(
                !err.0.contains("malformed JSON"),
                "{name} misdiagnosed an empty stdout: {err:?}",
            );
        }
    }

    /// Validation happens before any subprocess: an adapter pointed at a
    /// program that cannot exist proves nothing was executed.
    #[tokio::test]
    async fn malformed_arguments_are_rejected_before_execution() {
        let adapter = CliAdapter::new("must-not-execute");
        for (name, args) in [
            ("phux_status", json!({ "target": "@1" })),
            ("phux_doctor", json!({ "json": true })),
            // No multiplexer to address.
            ("phux_status", json!({ "action": "status" })),
            ("phux_doctor", json!({ "socket": 7 })),
            ("phux_diagnose", json!({})),
        ] {
            assert!(
                call_with_adapter(name, &args, &adapter).await.is_err(),
                "{name} accepted {args}",
            );
        }
        // `--remote` is a CLI flag, not an MCP argument.
        for args in [json!({ "remote": "me@mini" }), json!({ "socket": 7 })] {
            assert!(
                run_whoami(&args).await.is_err(),
                "phux_whoami accepted {args}"
            );
        }
    }
}
