//! Input schemas of the coordination tools.

use serde_json::{Value, json};

use crate::cli_tools::schema;

const ADDRESS_DESC: &str = "Speak as this address instead of this pane. An address is a pane \
    (`@749`), a labelled pane (`pi@749`), or a bare name for something outside a pane. On a host \
    joined to a hub, `hub/@749` is the hub's pane; on a hub, `min-api/@3` is a satellite's.";

/// Every schema in this family, in catalog order.
#[must_use]
pub(crate) fn schemas() -> Vec<Value> {
    vec![
        mail_send_schema(),
        mail_read_schema(),
        mail_ack_schema(),
        claim_take_schema(),
        claim_renew_schema(),
        claim_release_schema(),
        claim_list_schema(),
    ]
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
        "Send a message to other agents on this host, or across its hub. Mail is stored in the \
         host's work store, the hub's when this host joined one, and survives restarts. Nothing is typed into a recipient's terminal: its harness \
         integration puts the mail in the agent's conversation, at once for Pi, OMP and \
         OpenCode, at the agent's next hook for Claude Code and Grok. Address agents by pane \
         (`@749`) or labelled pane (`pi@749`), led by `hub/` or a satellite name such as \
         `min-api/` for one on another host; find them with phux_agent_list. You speak as \
         your own pane. Reply in the same `thread`. Result: `{schema: \"phux.coord/v1\", mail: \
         {id, thread, from, to, body, created_at}, operation_id, scope, replayed}`. \
         Supply operation_id for durable identical retries in this work store. It is separate \
         from idempotency_key, which owns only this adapter invocation. After an uncertain \
         result, a new adapter key may observe an explicit replay with the same mail \
         operation_id and identical sender/recipients/thread/body; never silently mint a new durable id.",
        json!({
            "to": { "type": "array", "minItems": 1, "maxItems": 32, "items": address(), "description": "Recipients." },
            "thread": thread(),
            "message": { "type": "string", "minLength": 1, "maxLength": 65536 },
            "operation_id": { "type": "string", "minLength": 32, "maxLength": 32, "pattern": "^[0-9a-fA-F]{32}$", "not": { "const": "00000000000000000000000000000000" }, "description": "Durable mail operation identity in this work store; reuse only with identical semantic arguments. If omitted, generated once and returned on success or uncertain failure. Not the MCP adapter idempotency_key." },
            "as": address(),
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
         exclusive claim (default) conflicts with any overlapping acquisition; \
         `shared` claims coexist. Taking never renews an existing lease, even at the same \
         address. Every path in one acquisition shares its fence. A CONFLICT IS AN \
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

fn fence_schema() -> Value {
    json!({"type": "integer", "minimum": 1, "maximum": i64::MAX, "description": "Exact fence returned by the acquisition, shared by all its paths."})
}

fn claim_renew_schema() -> Value {
    schema(
        "phux_claim_renew",
        "Renew precisely the live acquisition at this holder and fence. Paths, sharing mode \
         and note are unchanged; its whole path set renews atomically. An expired, foreign \
         or superseded receipt fails with claim_not_found and never acquires anew. \
         Result: {schema: phux.coord/v1, holder, claims: [...] }.",
        json!({
            "fence": fence_schema(),
            "ttl_secs": {"type": "integer", "minimum": 1, "maximum": 86400},
            "as": address(),
        }),
        &["fence"],
    )
}

fn claim_release_schema() -> Value {
    schema(
        "phux_claim_release",
        "Release precisely the acquisition at this holder and fence, including every path \
         it acquired. Stale or repeated cleanup returns released=0 and cannot remove a \
         replacement. No name-only, path-only or release-all mutation is supported. \
         Result: {schema: phux.coord/v1, holder, released}.",
        json!({"fence": fence_schema(), "as": address()}),
        &["fence"],
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
