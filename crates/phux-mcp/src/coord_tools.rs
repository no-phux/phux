//! Agent coordination over MCP (ADR-0187): `phux_mail_send`,
//! `phux_mail_read`, `phux_mail_ack`, `phux_claim_take`,
//! `phux_claim_renew`, `phux_claim_release`, and `phux_claim_list`.
//!
//! Each calls this host's work store in-process through
//! [`phux_work::Client`] and returns the same `phux.coord/v1` document as the
//! CLI verb it mirrors. The adapter runs in the agent's pane, so it speaks as
//! that pane unless `as` says otherwise ([`phux_work::caller_address`]).

use std::path::Path;

use phux_work::{
    AckMail, ClaimFilter, Client, DEFAULT_CLAIM_SECS, Endpoint, Launcher, OpaqueId, OperationId,
    ReadMail, ReleaseClaim, RenewClaim, SendMail, TakeClaim, caller_address, claim_path,
};
use serde_json::{Value, json};

use crate::tools::{ToolError, strict_object};

mod schemas;
pub(crate) use schemas::schemas;

/// Schema of every coordination document.
const SCHEMA: &str = "phux.coord/v1";

/// Whether `name` belongs to this family.
#[must_use]
pub(crate) fn owns(name: &str) -> bool {
    matches!(
        name,
        "phux_mail_send"
            | "phux_mail_read"
            | "phux_mail_ack"
            | "phux_claim_take"
            | "phux_claim_renew"
            | "phux_claim_release"
            | "phux_claim_list"
    )
}

/// Dispatch one coordination call.
///
/// # Errors
///
/// An unknown name, a malformed argument, or the store's refusal, carrying
/// the CLI's `phux.coord/v1` error document.
pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    let operation = Operation::parse(name, args)?;
    let client = store();
    tokio::task::spawn_blocking(move || operation.run(&client))
        .await
        .map_err(|err| ToolError::new(format!("work store call did not finish: {err}")))?
}

/// This host's work store, starting its keeper with the `phux` binary when
/// none answers, exactly as the CLI does.
fn store() -> Client {
    let endpoint = Endpoint::new(
        &phux_config::instance::state_dir(),
        &phux_config::instance::runtime_dir(),
    );
    let log = endpoint.store.parent().map(|dir| dir.join("keeper.log"));
    let launcher = Launcher {
        program: crate::cli_adapter::phux_program().into(),
        args: vec!["mission".into(), "keeper".into()],
        log,
    };
    Client::new(endpoint, Some(launcher))
}

/// One validated call, ready to run against the store.
#[derive(Debug)]
enum Operation {
    Send(SendMail),
    Read(ReadMail),
    Ack(AckMail),
    Take(TakeClaim),
    Renew(RenewClaim),
    Release(ReleaseClaim),
    List(ClaimFilter),
}

impl Operation {
    fn parse(name: &str, args: &Value) -> Result<Self, ToolError> {
        match name {
            "phux_mail_send" => mail_send(args),
            "phux_mail_read" => mail_read(args),
            "phux_mail_ack" => mail_ack(args),
            "phux_claim_take" => claim_take(args),
            "phux_claim_renew" => claim_renew(args),
            "phux_claim_release" => claim_release(args),
            "phux_claim_list" => claim_list(args),
            other => Err(ToolError::new(format!(
                "unknown coordination tool: {other}"
            ))),
        }
    }

    fn run(self, client: &Client) -> Result<Value, ToolError> {
        let document = match self {
            Self::Send(request) => client
                .send_mail(request)
                .map(|sent| json!({"schema": SCHEMA, "mail": sent.mail})),
            Self::Read(request) => {
                let reader = request.reader.clone();
                client
                    .read_mail(request)
                    .map(|mail| json!({"schema": SCHEMA, "reader": reader, "mail": mail}))
            }
            Self::Ack(request) => client
                .ack_mail(request)
                .map(|mail| json!({"schema": SCHEMA, "mail": mail})),
            Self::Take(request) => {
                let holder = request.holder.clone();
                client
                    .take_claim(request)
                    .map(|claim| json!({"schema": SCHEMA, "holder": holder, "claim": claim}))
            }
            Self::Renew(request) => {
                let holder = request.holder.clone();
                client
                    .renew_claim(request)
                    .map(|claims| json!({"schema": SCHEMA, "holder": holder, "claims": claims}))
            }
            Self::Release(request) => {
                let holder = request.holder.clone();
                client.release_claim(request).map(
                    |released| json!({"schema": SCHEMA, "holder": holder, "released": released}),
                )
            }
            Self::List(filter) => client
                .claims(filter)
                .map(|claims| json!({"schema": SCHEMA, "claims": claims})),
        };
        document.map_err(|err| failure(&err))
    }
}

/// The CLI's error document for `err`: the same code, remedy and exit.
fn failure(err: &phux_work::Error) -> ToolError {
    use phux_work::Error;
    let retry = "retry; if it persists, check `keeper.log` beside the work store";
    let (code, exit, remedy) = match err {
        Error::NoMail { .. } => (
            "mail_not_found",
            1,
            "check the id with `phux mail read --all`",
        ),
        Error::NoClaim { .. } => (
            "claim_not_found",
            1,
            "inspect `phux claim list`; acquire anew rather than renewing an expired receipt",
        ),
        Error::OperationConflict { .. } => (
            "mission_operation_conflict",
            1,
            "use a new operation id; this id already names a different write",
        ),
        Error::UnsupportedSchema { .. } => (
            "mission_unsupported_schema",
            1,
            "this binary reads mission schema version 2 only",
        ),
        Error::Busy => ("mission_busy", 1, retry),
        Error::Keeper(_) => ("mission_store", 1, retry),
        Error::InvalidAlias(_)
        | Error::InvalidOperationId(_)
        | Error::InvalidRepo(_)
        | Error::InvalidText { .. }
        | Error::EmptyEdit => (
            "usage_error",
            2,
            "run `phux mail --help` or `phux claim --help` for usage",
        ),
        _ => ("mission_store", 1, "check permissions on the work store"),
    };
    let message = err.to_string();
    let document = json!({
        "schema": SCHEMA,
        "schema_version": 1,
        "error": {"code": code, "message": message},
        "remedy": remedy,
        "exit_code": exit,
    });
    ToolError::with_document(message, document)
}

fn string<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(ToolError::new(format!("`{key}` must be a string"))),
    }
}

fn strings(args: &Value, key: &str) -> Result<Vec<String>, ToolError> {
    let invalid = || ToolError::new(format!("`{key}` must be an array of strings"));
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned).ok_or_else(invalid))
            .collect(),
        Some(_) => Err(invalid()),
    }
}

fn flag(args: &Value, key: &str) -> Result<bool, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ToolError::new(format!("`{key}` must be a boolean"))),
    }
}

/// Who this call speaks as: `as`, else the adapter's pane.
fn me(args: &Value) -> Result<String, ToolError> {
    Ok(caller_address(string(args, "as")?))
}

fn ttl(args: &Value) -> Result<u64, ToolError> {
    args.get("ttl_secs").map_or(Ok(DEFAULT_CLAIM_SECS), |ttl| {
        ttl.as_u64()
            .ok_or_else(|| ToolError::new("`ttl_secs` must be a positive integer"))
    })
}

fn fence(args: &Value) -> Result<u64, ToolError> {
    args.get("fence")
        .and_then(Value::as_u64)
        .filter(|fence| *fence > 0 && i64::try_from(*fence).is_ok())
        .ok_or_else(|| {
            ToolError::new("`fence` must be a positive signed 64-bit integer from acquisition")
        })
}

fn mail_send(args: &Value) -> Result<Operation, ToolError> {
    strict_object(
        args,
        &["to", "thread", "message", "as"],
        &["to", "thread", "message"],
    )?;
    let to = strings(args, "to")?;
    if to.is_empty() {
        return Err(ToolError::new("`to` names at least one recipient"));
    }
    let operation_id = OperationId::generate().map_err(|err| failure(&err))?;
    Ok(Operation::Send(SendMail {
        thread: string(args, "thread")?.unwrap_or_default().to_owned(),
        from: me(args)?,
        to,
        body: string(args, "message")?.unwrap_or_default().to_owned(),
        operation_id,
    }))
}

fn mail_read(args: &Value) -> Result<Operation, ToolError> {
    strict_object(args, &["thread", "all", "peek", "as"], &[])?;
    Ok(Operation::Read(ReadMail {
        reader: me(args)?,
        thread: string(args, "thread")?.map(str::to_owned),
        all: flag(args, "all")?,
        mark_read: !flag(args, "peek")?,
        limit: 0,
    }))
}

fn mail_ack(args: &Value) -> Result<Operation, ToolError> {
    strict_object(args, &["id", "as"], &["id"])?;
    let id = string(args, "id")?.unwrap_or_default();
    let id: OpaqueId = serde_json::from_value(Value::from(id)).map_err(|_| {
        ToolError::new(format!(
            "not a message id: {id}; use the 32 hex digits phux_mail_read returns"
        ))
    })?;
    Ok(Operation::Ack(AckMail {
        reader: me(args)?,
        id,
    }))
}

fn claim_take(args: &Value) -> Result<Operation, ToolError> {
    strict_object(
        args,
        &["paths", "shared", "ttl_secs", "note", "as"],
        &["paths"],
    )?;
    let paths = strings(args, "paths")?;
    if paths.is_empty() {
        return Err(ToolError::new("`paths` names at least one path"));
    }
    Ok(Operation::Take(TakeClaim {
        holder: me(args)?,
        paths: paths
            .iter()
            .map(|path| absolute(path))
            .collect::<Result<_, _>>()?,
        exclusive: !flag(args, "shared")?,
        ttl_secs: ttl(args)?,
        note: string(args, "note")?.unwrap_or_default().to_owned(),
    }))
}

fn claim_renew(args: &Value) -> Result<Operation, ToolError> {
    strict_object(args, &["fence", "ttl_secs", "as"], &["fence"])?;
    Ok(Operation::Renew(RenewClaim {
        holder: me(args)?,
        fence: fence(args)?,
        ttl_secs: ttl(args)?,
    }))
}

fn claim_release(args: &Value) -> Result<Operation, ToolError> {
    strict_object(args, &["fence", "as"], &["fence"])?;
    Ok(Operation::Release(ReleaseClaim {
        holder: me(args)?,
        fence: fence(args)?,
    }))
}

fn claim_list(args: &Value) -> Result<Operation, ToolError> {
    strict_object(args, &["path", "holder"], &[])?;
    Ok(Operation::List(ClaimFilter {
        path: string(args, "path")?.map(absolute).transpose()?,
        holder: string(args, "holder")?.map(str::to_owned),
    }))
}

/// A claimed path, normalized. The adapter's working directory is not the
/// agent's, so a relative path would claim the wrong place.
fn absolute(path: &str) -> Result<String, ToolError> {
    claim_path(Path::new(path)).ok_or_else(|| {
        ToolError::new(format!(
            "`{path}` is relative; pass an absolute path, since this adapter's working directory is not yours"
        ))
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn parse(name: &str, args: &Value) -> Result<Operation, ToolError> {
        Operation::parse(name, args)
    }

    #[test]
    fn mail_send_takes_the_text_as_given_and_refuses_the_doorbell_fields() {
        let Operation::Send(send) = parse(
            "phux_mail_send",
            &json!({"to": ["-x"], "thread": "t", "message": "--help", "as": "pi@749"}),
        )
        .unwrap() else {
            unreachable!("a send");
        };
        assert_eq!(
            (send.to, send.body, send.from),
            (
                vec!["-x".to_owned()],
                "--help".to_owned(),
                "pi@749".to_owned()
            )
        );
        for field in ["ring", "socket"] {
            let mut args = json!({"to": ["@7"], "thread": "t", "message": "m"});
            args[field] = json!(false);
            assert!(parse("phux_mail_send", &args).is_err(), "{field}");
        }
    }

    #[test]
    fn claims_refuse_relative_paths_and_unknown_fields_and_normalize_the_rest() {
        assert!(parse("phux_claim_take", &json!({"paths": ["rel"]})).is_err());
        assert!(parse("phux_claim_take", &json!({"paths": ["/a"], "bogus": 1})).is_err());
        let Operation::Take(take) = parse(
            "phux_claim_take",
            &json!({"paths": ["/a/./b/../c"], "shared": true, "ttl_secs": 60, "as": "pi@749"}),
        )
        .unwrap() else {
            unreachable!("a take");
        };
        assert_eq!(take.paths, ["/a/c"]);
        assert!(!take.exclusive);
        assert_eq!((take.ttl_secs, take.holder.as_str()), (60, "pi@749"));
    }

    #[test]
    fn lease_mutations_require_the_exact_receipt_and_reject_unsafe_forms() {
        for tool in ["phux_claim_renew", "phux_claim_release"] {
            for bad in [
                json!({}),
                json!({"fence": 0}),
                json!({"fence": -1}),
                json!({"fence": "1"}),
                json!({"fence": 1.5}),
                json!({"fence": u64::MAX}),
                json!({"paths": ["/a"]}),
                json!({"fence": 1, "paths": []}),
            ] {
                assert!(parse(tool, &bad).is_err(), "{tool}: {bad}");
            }
        }
        let Operation::Renew(renew) =
            parse("phux_claim_renew", &json!({"fence": 12, "as": "@749"})).unwrap()
        else {
            unreachable!("a renewal");
        };
        assert_eq!((renew.fence, renew.ttl_secs), (12, DEFAULT_CLAIM_SECS));
        assert!(parse("phux_claim_release", &json!({"fence": 12, "ttl_secs": 60})).is_err());
    }

    #[test]
    fn store_failures_carry_the_cli_error_document() {
        let document = failure(&phux_work::Error::NoMail {
            id: "00".into(),
            reader: "@7".into(),
        })
        .document();
        assert_eq!(document["schema"], SCHEMA);
        assert_eq!(document["error"]["code"], "mail_not_found");
        assert_eq!(document["exit_code"], 1);
        let usage = failure(&phux_work::Error::InvalidText {
            field: "address".into(),
            reason: "bad".into(),
        })
        .document();
        assert_eq!(
            (usage["error"]["code"].as_str(), usage["exit_code"].as_u64()),
            (Some("usage_error"), Some(2))
        );
    }

    #[test]
    fn every_schema_is_owned_and_dispatched() {
        for schema in schemas() {
            let name = schema["name"].as_str().unwrap();
            assert!(owns(name), "{name}");
            assert!(
                !matches!(parse(name, &json!({})), Err(err) if err.to_string().contains("unknown coordination tool")),
                "{name}"
            );
        }
    }
}
