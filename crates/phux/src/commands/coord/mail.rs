//! Mail submission and observation over the durable work-store client.
use super::{SCHEMA, emit, store, usage_error};
use phux_work::{AckMail, ReadMail, SendMail};
use std::io::Read as _;
use std::process::ExitCode;

pub(super) fn send(
    to: &[String],
    message: &str,
    thread: &str,
    me: &str,
    json: bool,
    operation: Option<&str>,
) -> ExitCode {
    let operation_id = match operation.map_or_else(
        phux_work::OperationId::generate,
        phux_work::OperationId::parse,
    ) {
        Ok(id) => id,
        Err(err) => return crate::commands::mission::report_as(json, &err, SCHEMA),
    };
    let body = if message == "-" {
        let mut text = String::new();
        if let Err(err) = std::io::stdin().read_to_string(&mut text) {
            return usage_error(json, format!("could not read stdin: {err}"), "pass -m TEXT");
        }
        text
    } else {
        message.to_owned()
    };
    let client = match store(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let sent = match client.send_mail(SendMail {
        thread: thread.to_owned(),
        from: me.to_owned(),
        to: to.to_vec(),
        body,
        operation_id,
    }) {
        Ok(sent) => sent,
        Err(err) => {
            use phux_client::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};
            let certainty = if err.write_outcome_unknown() {
                EffectCertainty::Unknown
            } else {
                EffectCertainty::NotApplied
            };
            let recovery = Recovery::new(
                ScopeKind::WorkStore,
                Some(operation_id.to_hex()),
                certainty,
                NextAction::Reconcile,
            );
            return crate::commands::mission::report_with_recovery(
                json,
                &err,
                SCHEMA,
                Some(&recovery),
            );
        }
    };
    if json {
        emit(&serde_json::json!({"schema": SCHEMA, "mail": sent.mail,
            "operation_id": operation_id.to_hex(), "scope": phux_client::recovery::Scope { kind: phux_client::recovery::ScopeKind::WorkStore, id: None },
            "replayed": sent.idempotent_replay}));
    } else {
        outln!(
            "sent {} to {} (operation {})",
            sent.mail.id.to_hex(),
            sent.mail.to.join(", "),
            operation_id.to_hex()
        );
    }
    ExitCode::SUCCESS
}

pub(super) fn read(
    thread: Option<String>,
    all: bool,
    peek: bool,
    me: &str,
    json: bool,
) -> ExitCode {
    let client = match store(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let mail = match client.read_mail(ReadMail {
        reader: me.to_owned(),
        thread,
        all,
        mark_read: !peek,
        limit: 0,
    }) {
        Ok(mail) => mail,
        Err(err) => return crate::commands::mission::report_as(json, &err, SCHEMA),
    };
    if json {
        emit(&serde_json::json!({"schema": SCHEMA, "reader": me, "mail": mail}));
    } else if mail.is_empty() {
        outln!("no mail for {me}");
    } else {
        for message in &mail {
            outln!(
                "{}  {}  {} -> {}  [{}]{}",
                message.created_at,
                message.id.to_hex(),
                message.from,
                message.to.join(", "),
                message.thread,
                if message.acked_at.is_some() {
                    "  acked"
                } else {
                    ""
                }
            );
            for line in message.body.lines() {
                outln!("    {line}");
            }
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn ack(id: &str, me: &str, json: bool) -> ExitCode {
    let Ok(id) = serde_json::from_value(serde_json::Value::from(id)) else {
        return usage_error(
            json,
            format!("not a message id: {id}"),
            "use the 32 hex digits `phux mail read` prints",
        );
    };
    let client = match store(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    match client.ack_mail(AckMail {
        reader: me.to_owned(),
        id,
    }) {
        Ok(mail) => {
            if json {
                emit(&serde_json::json!({"schema": SCHEMA, "mail": mail}));
            } else {
                outln!("acknowledged {} from {}", mail.id.to_hex(), mail.from);
            }
            ExitCode::SUCCESS
        }
        Err(err) => crate::commands::mission::report_as(json, &err, SCHEMA),
    }
}
