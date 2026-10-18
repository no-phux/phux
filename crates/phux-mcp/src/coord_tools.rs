//! Agent coordination over MCP (ADR-0187): `phux_mail_send`,
//! `phux_mail_read`, `phux_mail_ack`, `phux_claim_take`,
//! `phux_claim_release`, and `phux_claim_list`. Each is a transport for
//! the canonical CLI's `phux.coord/v1` document, so the work store has one
//! client and one identity rule: the adapter runs in the agent's pane and
//! speaks as that pane unless `as` says otherwise.

#![allow(
    clippy::similar_names,
    reason = "argv and parsed args are deliberately adjacent in thin CLI wrappers"
)]

use serde_json::{Value, json};

use crate::cli_adapter::{CliAdapter, DEFAULT_CALL_TIMEOUT, push_socket};
use crate::cli_tools::schema;
use crate::tools::{ToolError, strict_object};

/// The exit `claim take` spends on a conflict, with the document on stdout.
const EXIT_CONFLICT: i32 = 1;

const ADDRESS_DESC: &str = "Speak as this address instead of this pane. An address is a pane \
    (`@749`), a labelled pane (`pi@749`), or a bare name for something outside a pane.";

/// Every schema in this family, in catalog order.
#[must_use]
pub(crate) fn schemas() -> Vec<Value> {
    vec![
        mail_send_schema(),
        mail_read_schema(),
        mail_ack_schema(),
        claim_take_schema(),
        claim_release_schema(),
        claim_list_schema(),
    ]
}

/// Whether `name` belongs to this family.
#[must_use]
pub(crate) fn owns(name: &str) -> bool {
    matches!(
        name,
        "phux_mail_send"
            | "phux_mail_read"
            | "phux_mail_ack"
            | "phux_claim_take"
            | "phux_claim_release"
            | "phux_claim_list"
    )
}

/// Dispatch one coordination call.
///
/// # Errors
///
/// An unknown name, a malformed argument, or a CLI failure with no document.
pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    call_with_adapter(name, args, &CliAdapter::for_residue(name)?).await
}

async fn call_with_adapter(
    name: &str,
    args: &Value,
    adapter: &CliAdapter,
) -> Result<Value, ToolError> {
    let argv = argv(name, args)?;
    if name == "phux_claim_take" {
        let output = adapter
            .run_allowing(argv, DEFAULT_CALL_TIMEOUT, &[EXIT_CONFLICT])
            .await?;
        if output.stdout.trim().is_empty() {
            return Err(ToolError::new(output.stderr.trim().to_owned()));
        }
        return serde_json::from_str(&output.stdout).map_err(|err| {
            ToolError::new(format!("phux claim take returned malformed JSON: {err}"))
        });
    }
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

fn string<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(ToolError::new(format!("`{key}` must be a string"))),
    }
}

fn strings(args: &Value, key: &str) -> Result<Vec<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| ToolError::new(format!("`{key}` must be an array of strings")))
            })
            .collect(),
        Some(_) => Err(ToolError::new(format!(
            "`{key}` must be an array of strings"
        ))),
    }
}

fn flag(args: &Value, key: &str) -> Result<bool, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ToolError::new(format!("`{key}` must be a boolean"))),
    }
}

/// `--as`, appended when `as` is set.
fn push_as(argv: &mut Vec<String>, args: &Value) -> Result<(), ToolError> {
    if let Some(me) = string(args, "as")? {
        argv.extend(["--as".to_owned(), me.to_owned()]);
    }
    Ok(())
}

/// The CLI argv for one call. Positional values go after `--`, so a value
/// that starts with `-` is never read as a flag.
fn argv(name: &str, args: &Value) -> Result<Vec<String>, ToolError> {
    match name {
        "phux_mail_send" => mail_send_argv(args),
        "phux_mail_read" => mail_read_argv(args),
        "phux_mail_ack" => {
            strict_object(args, &["id", "as"], &["id"])?;
            let mut argv = vec!["mail".into(), "ack".into(), "--json".into()];
            push_as(&mut argv, args)?;
            argv.push("--".into());
            argv.push(string(args, "id")?.unwrap_or_default().to_owned());
            Ok(argv)
        }
        "phux_claim_take" => claim_take_argv(args),
        "phux_claim_release" => {
            strict_object(args, &["paths", "as"], &[])?;
            let mut argv = vec!["claim".into(), "release".into(), "--json".into()];
            push_as(&mut argv, args)?;
            argv.push("--".into());
            argv.extend(absolute_paths(args)?);
            Ok(argv)
        }
        "phux_claim_list" => claim_list_argv(args),
        other => Err(ToolError::new(format!(
            "unknown coordination tool: {other}"
        ))),
    }
}

fn mail_send_argv(args: &Value) -> Result<Vec<String>, ToolError> {
    strict_object(
        args,
        &["to", "thread", "message", "ring", "as", "socket"],
        &["to", "thread", "message"],
    )?;
    let to = strings(args, "to")?;
    if to.is_empty() {
        return Err(ToolError::new("`to` names at least one recipient"));
    }
    let message = string(args, "message")?.unwrap_or_default();
    if message == "-" {
        return Err(ToolError::new(
            "`message` is the text itself; `-` (stdin) is CLI-only",
        ));
    }
    let thread = string(args, "thread")?.unwrap_or_default();
    let mut argv: Vec<String> = vec!["mail".into(), "send".into(), "--json".into()];
    argv.extend(["--thread".into(), thread.to_owned()]);
    argv.extend(["--message".into(), message.to_owned()]);
    if args.get("ring") == Some(&Value::Bool(false)) {
        argv.push("--no-ring".into());
    }
    push_as(&mut argv, args)?;
    push_socket(&mut argv, args)?;
    argv.push("--".into());
    argv.extend(to);
    Ok(argv)
}

fn mail_read_argv(args: &Value) -> Result<Vec<String>, ToolError> {
    strict_object(args, &["thread", "all", "peek", "as"], &[])?;
    let mut argv: Vec<String> = vec!["mail".into(), "read".into(), "--json".into()];
    if let Some(thread) = string(args, "thread")? {
        argv.extend(["--thread".into(), thread.to_owned()]);
    }
    if flag(args, "all")? {
        argv.push("--all".into());
    }
    if flag(args, "peek")? {
        argv.push("--peek".into());
    }
    push_as(&mut argv, args)?;
    Ok(argv)
}

fn claim_take_argv(args: &Value) -> Result<Vec<String>, ToolError> {
    strict_object(
        args,
        &["paths", "shared", "ttl_secs", "note", "as"],
        &["paths"],
    )?;
    let paths = absolute_paths(args)?;
    if paths.is_empty() {
        return Err(ToolError::new("`paths` names at least one path"));
    }
    let mut argv: Vec<String> = vec!["claim".into(), "take".into(), "--json".into()];
    if flag(args, "shared")? {
        argv.push("--shared".into());
    }
    if let Some(ttl) = args.get("ttl_secs") {
        let ttl = ttl
            .as_u64()
            .ok_or_else(|| ToolError::new("`ttl_secs` must be a positive integer"))?;
        argv.extend(["--ttl".into(), ttl.to_string()]);
    }
    if let Some(note) = string(args, "note")? {
        argv.extend(["--note".into(), note.to_owned()]);
    }
    push_as(&mut argv, args)?;
    argv.push("--".into());
    argv.extend(paths);
    Ok(argv)
}

fn claim_list_argv(args: &Value) -> Result<Vec<String>, ToolError> {
    strict_object(args, &["path", "holder"], &[])?;
    let mut argv: Vec<String> = vec!["claim".into(), "list".into(), "--json".into()];
    if let Some(path) = string(args, "path")? {
        require_absolute(path)?;
        argv.extend(["--path".into(), path.to_owned()]);
    }
    if let Some(holder) = string(args, "holder")? {
        argv.extend(["--holder".into(), holder.to_owned()]);
    }
    Ok(argv)
}

/// The adapter's working directory is not the agent's, so a relative path
/// would claim the wrong place.
fn require_absolute(path: &str) -> Result<(), ToolError> {
    if path.starts_with('/') {
        Ok(())
    } else {
        Err(ToolError::new(format!(
            "`{path}` is relative; pass an absolute path, since this adapter's working directory is not yours"
        )))
    }
}

fn absolute_paths(args: &Value) -> Result<Vec<String>, ToolError> {
    let paths = strings(args, "paths")?;
    for path in &paths {
        require_absolute(path)?;
    }
    Ok(paths)
}

fn address() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": 128, "description": ADDRESS_DESC })
}

fn thread() -> Value {
    json!({
        "type": "string", "minLength": 1, "maxLength": 64, "pattern": "^[A-Za-z0-9._-]+$",
        "description": "Thread slug: the branch, PR or task the message is about."
    })
}

fn path_list(description: &str) -> Value {
    json!({
        "type": "array", "maxItems": 64,
        "items": { "type": "string", "minLength": 2, "maxLength": 4096, "pattern": "^/" },
        "description": description,
    })
}

fn mail_send_schema() -> Value {
    schema(
        "phux_mail_send",
        "Send a message to other agents and ring their panes. Mail is stored in this host's work \
         store first and survives restarts; then each recipient pane that hosts an agent gets one \
         acknowledged single-line prompt naming you and the thread, so an idle agent wakes and \
         reads. A pane with no agent, or no reachable server, is not rung and the mail is still \
         delivered; `bells` reports each outcome. Address agents by pane (`@749`) or labelled \
         pane (`pi@749`); find them with phux_agent_list. You speak as your own pane. Reply in \
         the same `thread`. Result: `{schema: \"phux.coord/v1\", mail: {id, thread, from, to, \
         body, created_at}, bells: [{to, outcome}]}`.",
        json!({
            "to": { "type": "array", "minItems": 1, "maxItems": 32, "items": address(), "description": "Recipients." },
            "thread": thread(),
            "message": { "type": "string", "minLength": 1, "maxLength": 65536 },
            "ring": { "type": "boolean", "description": "Ring recipient panes. Default true." },
            "as": address(),
            "socket": { "type": "string", "minLength": 1, "maxLength": 4096 },
        }),
        &["to", "thread", "message"],
    )
}

fn mail_read_schema() -> Value {
    schema(
        "phux_mail_read",
        "Read your mail, oldest first, and mark it read. By default only unread messages sent to \
         you; `thread` narrows to one thread and then also shows what you sent there; `all` \
         includes messages already read; `peek` leaves them unread. Result: `{schema: \
         \"phux.coord/v1\", reader, mail: [{id, thread, from, to, body, created_at, read_at, \
         acked_at}]}`. Acknowledge with phux_mail_ack when you have handled one, so the sender \
         knows.",
        json!({
            "thread": thread(),
            "all": { "type": "boolean" },
            "peek": { "type": "boolean" },
            "as": address(),
        }),
        &[],
    )
}

fn mail_ack_schema() -> Value {
    schema(
        "phux_mail_ack",
        "Acknowledge a message sent to you, which also marks it read. Repeating is harmless. \
         Fails with `mail_not_found` when the message was not sent to you.",
        json!({
            "id": { "type": "string", "pattern": "^[0-9a-f]{32}$", "description": "Message id from phux_mail_read." },
            "as": address(),
        }),
        &["id"],
    )
}

fn claim_take_schema() -> Value {
    schema(
        "phux_claim_take",
        "Claim absolute paths before working there, so other agents know to leave them alone. A \
         claim covers its path and everything under it. All paths are claimed or none. An \
         exclusive claim (default) conflicts with any other holder's overlapping claim; \
         `shared` claims coexist. Claiming again renews and keeps the fence. A CONFLICT IS AN \
         ANSWER, NOT AN ERROR: branch on `claim.outcome` (`granted` or `conflict`); a conflict \
         lists the holders, so mail them. Claims are advisory and lapse after `ttl_secs` \
         (default 3600, at most 86400). Result: `{schema: \"phux.coord/v1\", holder, claim: \
         {outcome, claims: [{path, holder, exclusive, fence, note, expires_at}]}}`.",
        json!({
            "paths": path_list("Absolute paths to claim."),
            "shared": { "type": "boolean" },
            "ttl_secs": { "type": "integer", "minimum": 1, "maximum": 86400 },
            "note": { "type": "string", "maxLength": 512, "description": "Why, shown to anyone it blocks." },
            "as": address(),
        }),
        &["paths"],
    )
}

fn claim_release_schema() -> Value {
    schema(
        "phux_claim_release",
        "Release your claims on exactly these absolute paths, or all of yours when `paths` is \
         empty. Result: `{schema: \"phux.coord/v1\", holder, released}`.",
        json!({
            "paths": path_list("Exact paths you claimed. Empty releases all of yours."),
            "as": address(),
        }),
        &[],
    )
}

fn claim_list_schema() -> Value {
    schema(
        "phux_claim_list",
        "List live claims, optionally only those overlapping an absolute `path` or held by \
         `holder`. READ-ONLY. Check before starting work in a shared checkout. Result: \
         `{schema: \"phux.coord/v1\", claims: [...]}`.",
        json!({
            "path": { "type": "string", "minLength": 2, "maxLength": 4096, "pattern": "^/" },
            "holder": address(),
        }),
        &[],
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn positional_values_follow_a_separator() {
        let argv = argv(
            "phux_mail_send",
            &json!({"to": ["-x"], "thread": "t", "message": "--help", "ring": false}),
        )
        .unwrap();
        assert_eq!(
            argv,
            [
                "mail",
                "send",
                "--json",
                "--thread",
                "t",
                "--message",
                "--help",
                "--no-ring",
                "--",
                "-x"
            ]
        );
    }

    #[test]
    fn claims_refuse_relative_paths_and_unknown_fields() {
        assert!(argv("phux_claim_take", &json!({"paths": ["rel"]})).is_err());
        assert!(argv("phux_claim_take", &json!({"paths": ["/a"], "bogus": 1})).is_err());
        assert_eq!(
            argv(
                "phux_claim_take",
                &json!({"paths": ["/a"], "shared": true, "ttl_secs": 60, "as": "pi@749"})
            )
            .unwrap(),
            [
                "claim", "take", "--json", "--shared", "--ttl", "60", "--as", "pi@749", "--", "/a"
            ]
        );
    }

    #[test]
    fn every_schema_is_owned_and_dispatched() {
        for schema in schemas() {
            let name = schema["name"].as_str().unwrap();
            assert!(owns(name), "{name}");
        }
    }
}
