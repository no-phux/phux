//! Conventional L3 metadata keys and their value codecs.

/// Conventional L3 metadata key renaming a session (ADR-0019 / ADR-0027): the
/// server intercepts a `SET_METADATA` of `current\0new` and applies the
/// registry rename.
pub const SESSION_NAME_KEY: &str = "phux.session.name/v1";

/// Encode a [`SESSION_NAME_KEY`] value: `current\0new`.
#[must_use]
pub fn encode_session_rename(current: &str, new_name: &str) -> Vec<u8> {
    let mut value = current.as_bytes().to_vec();
    value.push(0);
    value.extend_from_slice(new_name.as_bytes());
    value
}

/// Decode a [`SESSION_NAME_KEY`] value into `(current, new)`.
///
/// `None` for anything other than UTF-8 `current\0new` with both sides
/// non-empty and no extra NULs.
#[must_use]
pub fn decode_session_rename(value: &[u8]) -> Option<(&str, &str)> {
    let text = std::str::from_utf8(value).ok()?;
    let (current, new_name) = text.split_once('\0')?;
    if current.is_empty() || new_name.is_empty() || new_name.contains('\0') {
        return None;
    }
    Some((current, new_name))
}

/// Conventional L3 metadata key creating a named session without attaching.
///
/// ADR-0019 / ADR-0027. Value: UTF-8 JSON `{ "name", "command"?, "cwd"? }`
/// written under `Scope::Global`; the server seeds the session and pane as
/// `ATTACH { CreateIfMissing }` would and records the result.
pub const SESSION_CREATE_KEY: &str = "phux.session.create/v1";

/// Global key where the server publishes the latest [`SESSION_CREATE_KEY`]
/// result as JSON `{ "name", "terminal_id" }`, since `SET_METADATA` has no
/// reply frame.
pub const SESSION_CREATE_RESULT_KEY: &str = "phux.session.created/v1";

/// Prefix for one-shot session-create result keys: a client that sends a
/// `request_token` reads `"{prefix}{request_token}"`, consumed on first GET.
pub const SESSION_CREATE_RESULT_KEY_PREFIX: &str = "phux.session.created/v1/";

/// Global key setting a session's keep-empty mark (ADR-0105).
///
/// Value `name\0true` or `name\0false`, applied and broadcast, not stored. Clearing
/// the mark on a windowless session removes it. Gated on
/// [`ServerFeature::KeepEmptySessions`](crate::caps::ServerFeature::KeepEmptySessions).
pub const SESSION_KEEP_EMPTY_KEY: &str = "phux.session.keep_empty/v1";

/// Global key grouping sessions under a project tag.
///
/// Stored, not intercepted (`docs/spec/L3.md` §3.1). Value: `name\0project`,
/// both non-empty UTF-8. A `GET` returns the stored bytes. `DELETE` clears
/// the tag. Pickers group sessions that share the project side.
pub const SESSION_PROJECT_KEY: &str = "phux.session.project/v1";

/// Encode a [`SESSION_PROJECT_KEY`] value: `name\0project`.
#[must_use]
pub fn encode_session_project(name: &str, project: &str) -> Vec<u8> {
    encode_named_tag(name, project)
}

/// Decode a [`SESSION_PROJECT_KEY`] value into `(name, project)`.
///
/// `None` unless both sides are non-empty UTF-8 with no extra NULs.
#[must_use]
pub fn decode_session_project(value: &[u8]) -> Option<(&str, &str)> {
    decode_named_tag(value)
}

/// Global key for the host a session is organized on.
///
/// Stored, not intercepted (`docs/spec/L3.md` §3.1). Value: `name\0host`,
/// both non-empty UTF-8. A `GET` returns the stored bytes. `DELETE` clears
/// the tag. A focused satellite pane names its own host at save time; this
/// tag is the host recorded for a pane on the attached server.
pub const SESSION_HOST_KEY: &str = "phux.session.host/v1";

/// Encode a [`SESSION_HOST_KEY`] value: `name\0host`.
#[must_use]
pub fn encode_session_host(name: &str, host: &str) -> Vec<u8> {
    encode_named_tag(name, host)
}

/// Decode a [`SESSION_HOST_KEY`] value into `(name, host)`.
///
/// `None` unless both sides are non-empty UTF-8 with no extra NULs.
#[must_use]
pub fn decode_session_host(value: &[u8]) -> Option<(&str, &str)> {
    decode_named_tag(value)
}

/// Global key for the directory a session is organized in.
///
/// Stored, not intercepted (`docs/spec/L3.md` §3.1). Value: `name\0directory`,
/// both non-empty UTF-8. A `GET` returns the stored bytes. `DELETE` clears
/// the tag. Workspace save uses this tag when it is set, so a directory the
/// child could not enter still round-trips. The pane's own cwd stays the
/// directory the process actually started in.
pub const SESSION_DIRECTORY_KEY: &str = "phux.session.directory/v1";

/// Encode a [`SESSION_DIRECTORY_KEY`] value: `name\0directory`.
#[must_use]
pub fn encode_session_directory(name: &str, directory: &str) -> Vec<u8> {
    encode_named_tag(name, directory)
}

/// Decode a [`SESSION_DIRECTORY_KEY`] value into `(name, directory)`.
///
/// `None` unless both sides are non-empty UTF-8 with no extra NULs.
#[must_use]
pub fn decode_session_directory(value: &[u8]) -> Option<(&str, &str)> {
    decode_named_tag(value)
}

fn encode_named_tag(name: &str, value: &str) -> Vec<u8> {
    let mut bytes = name.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

fn decode_named_tag(value: &[u8]) -> Option<(&str, &str)> {
    let text = std::str::from_utf8(value).ok()?;
    let (name, rest) = text.split_once('\0')?;
    if name.is_empty() || rest.is_empty() || rest.contains('\0') {
        return None;
    }
    Some((name, rest))
}

/// Encode a [`SESSION_KEEP_EMPTY_KEY`] value: `name\0true` or `name\0false`.
#[must_use]
pub fn encode_session_keep_empty(name: &str, keep: bool) -> Vec<u8> {
    format!("{name}\0{keep}").into_bytes()
}

/// Decode a [`SESSION_KEEP_EMPTY_KEY`] value into the session name and the
/// mark. `None` for anything other than UTF-8 `name\0true` or `name\0false`.
#[must_use]
pub fn decode_session_keep_empty(value: &[u8]) -> Option<(&str, bool)> {
    let (name, flag) = std::str::from_utf8(value).ok()?.split_once('\0')?;
    match flag {
        "true" => Some((name, true)),
        "false" => Some((name, false)),
        _ => None,
    }
}

/// Terminal-scoped key holding freeform tags as a JSON array of unique,
/// non-empty strings (`docs/spec/L3.md` §3.6, ADR-0027). Stored opaquely.
pub const RESOURCE_TAGS_KEY: &str = "phux.tags/v1";

/// Terminal-scoped key holding outgoing link edges as a JSON array of
/// `{ "target": u32, "kind": str }`; `kind` is an open enum (ADR-0027).
/// Stored opaquely.
pub const RESOURCE_LINK_KEY: &str = "phux.link/v1";

/// Terminal-scoped key holding the declared agent identity and lifecycle.
///
/// `docs/spec/L3.md` §3.7, ADR-0040. A consumer that finds it MUST
/// prefer it over title or screen heuristics. Stored opaquely.
pub const RESOURCE_AGENT_KEY: &str = "phux.agent/v1";

/// Server-owned projection of a pending `AgentEvent::Asked` (ADR-0035, ADR-0136).
///
/// Value is the single byte `1` while any ask source still holds, and the
/// key is absent once none does. There is no wire event for a question
/// going away, so this tombstone is how a consumer learns the clear. A hub
/// copies the key read-only onto `Satellite` scopes; clients must not set
/// or delete it.
pub const RESOURCE_ASKED_KEY: &str = "phux.agent.asked/v1";

/// Conventional Terminal-scoped provenance for provider-native session resume.
///
/// Value: bounded UTF-8 JSON `{plugin_id, integration_id, native_id}` owned by
/// ADR-0068. `SPAWN_RESOURCE.agent_session` may install these opaque bytes
/// atomically with a local spawn; ordinary L3 reads and writes use this key.
pub const RESOURCE_AGENT_SESSION_KEY: &str = "phux.agent-session/v1";

/// Conventional Terminal-scoped metadata key for the server-observed
/// foreground process and available-shell answer. Clients may read and
/// subscribe to this key but MUST NOT set or delete it.
pub const RESOURCE_PANE_OCCUPANT_KEY: &str = "phux.pane-occupant/v1";

/// Server-owned OSC 7501 record index and active status (L3 §3.7.3).
pub const RESOURCE_PROGRAM_STATUS_KEY: &str = "phux.program-status/v1";

/// Server-owned OSC 7501 records; the empty suffix addresses the root.
pub const RESOURCE_PROGRAM_STATUS_RECORD_PREFIX: &str = "phux.program-status.record/v1/";

/// Maximum encoded `phux.agent-session/v1` record accepted by server mutations.
pub const MAX_AGENT_SESSION_RECORD_BYTES: usize = 4 * 1024;

/// Global config-reload doorbell.
///
/// The value is a fresh nonce (so SET dedup
/// does not swallow it); subscribers re-read their own local config on each
/// non-tombstone change and MUST keep the previous config if the re-read
/// fails. Config never crosses the wire.
pub const CONFIG_RELOAD_KEY: &str = "phux.config.reload/v1";

/// `Global`-scope key family of the server-owned approval records (ADR-0128).
///
/// `phux.approval/v1/<id>` holds one held action's JSON description
/// (`docs/spec/L3.md` §3.10) while it awaits a decision, and is deleted when
/// the action is decided, expires, or is withdrawn. No client may set or
/// delete a key in this family.
pub const APPROVAL_KEY_PREFIX: &str = "phux.approval/v1/";

/// `Global`-scope key family a decision is written to (ADR-0128).
///
/// `SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with value
/// `approve` or `deny`. The server intercepts the write, classifies it as
/// `SIGNAL` on the held action's subject, and stores nothing.
pub const APPROVAL_DECIDE_KEY_PREFIX: &str = "phux.approval.decide/v1/";

/// `Global`-scope key family a device registers a background-push grant
/// under (ADR-0155, `docs/spec/L3.md` §3.11).
///
/// `phux.push/v1/<device>` holds a client-owned UTF-8 JSON object naming the
/// gateway the device trusts, the bearer secret that gateway minted for it,
/// and the id the device knows this server by. The server posts a
/// content-free notice there when an agent asks while the connection that
/// wrote the key is gone.
pub const PUSH_GRANT_KEY_PREFIX: &str = "phux.push/v1/";

/// A decision's value: release the held action once.
pub const APPROVAL_APPROVE: &[u8] = b"approve";

/// A decision's value: refuse the held action.
pub const APPROVAL_DENY: &[u8] = b"deny";

/// Whether `value` is a decision a decide key accepts.
#[must_use]
pub fn is_approval_decision(value: &[u8]) -> bool {
    value == APPROVAL_APPROVE || value == APPROVAL_DENY
}
