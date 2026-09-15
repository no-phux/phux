//! In-process MCP tools over the pane-shaping `phux-client` homes:
//! `phux_spawn`, `phux_signal`, `phux_tag`, `phux_rename`, and the three
//! spatial edits.
//!
//! Each used to execute the canonical CLI; each now calls the library home
//! that CLI verb calls and returns the same document. Where the document is
//! a CLI `--json` document (spawn, the spatial edits) the library builds it
//! for both surfaces; the others are MCP envelopes this adapter always
//! built. The goldens in `crate::goldens` pin every one.

use std::path::Path;

use phux_client::attach::connection::Connection;
use phux_client::layout::SplitDir;
use phux_client::selector::{self, Selector, format_terminal_id};
use phux_client::signal::LeaseOutcome;
use phux_client::spatial::{Direction, SpatialError, SpatialOp};
use phux_client::spawn::{Placement, RollbackOutcome};
use phux_client::state;
use phux_client::tags::{TagSession, TagWriteOutcome};
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{GroupId, IdempotencyKey, ResourceId, SatelliteHost};
use phux_protocol::wire::frame::{
    FrameKind, SpawnError, SpawnResource, SpawnResult, TerminalSignal,
};
use phux_protocol::wire::info::SessionSnapshot;
use serde_json::{Value, json};

use crate::cli_adapter::{bounded_string, bounded_strings, enum_string, ratio};
use crate::cli_tools::optional_bool;
use crate::tools::{
    ToolError, contract_error, resolve_one_for_input, socket_arg, strict_object, transport_error,
};

/// The split ratio the CLI defaults `--ratio` to.
const DEFAULT_RATIO: f32 = 0.5;

/// `ratio` as the CLI parsed it: the argument's decimal text read as `f32`,
/// so an in-process call carries the exact value `--ratio N` used to.
fn cli_ratio(args: &Value) -> Result<f32, ToolError> {
    ratio(args)?.map_or(Ok(DEFAULT_RATIO), |value| {
        value
            .to_string()
            .parse::<f32>()
            .map_err(|err| ToolError::new(format!("`ratio` is not a float: {err}")))
    })
}

fn parse_selector(target: &str) -> Result<Selector, ToolError> {
    selector::parse(target)
        .map_err(|err| ToolError::new(format!("invalid target '{target}': {err}")))
}

// -----------------------------------------------------------------------------
// spawn
// -----------------------------------------------------------------------------

/// One validated `phux_spawn` request.
struct SpawnRequest {
    target: Option<String>,
    satellite: Option<String>,
    split: SplitDir,
    ratio: f32,
    projection: Option<String>,
    cwd: Option<String>,
    command: Vec<String>,
    retain_secs: Option<u32>,
    idempotency_key: Option<IdempotencyKey>,
}

impl SpawnRequest {
    fn parse(args: &Value) -> Result<Self, ToolError> {
        let target = bounded_string(args, "target", false)?;
        let satellite = bounded_string(args, "satellite", false)?;
        let retain_secs = retain_secs(args)?;
        let idempotency_key = bounded_string(args, "idempotency_key", false)?
            .as_deref()
            .map(idempotency_key)
            .transpose()?;
        if target.is_some() && satellite.is_some() {
            return Err(ToolError::new("`target` conflicts with `satellite`"));
        }
        let split = enum_string(
            args,
            "split",
            &["horizontal", "vertical"],
            Some("horizontal"),
        )?;
        let ratio = cli_ratio(args)?;
        let projection = bounded_string(args, "projection", false)?;
        let placed =
            args.get("split").is_some() || args.get("ratio").is_some() || projection.is_some();
        if target.is_none() && placed {
            return Err(ToolError::new(
                "`split`, `ratio`, and `projection` require `target`",
            ));
        }
        Ok(Self {
            target,
            satellite,
            // `phux spawn --split` names the child axis directly.
            split: if split == "vertical" {
                SplitDir::Vertical
            } else {
                SplitDir::Horizontal
            },
            ratio,
            projection,
            cwd: bounded_string(args, "cwd", false)?,
            command: bounded_strings(args, "command", false)?,
            retain_secs,
            idempotency_key,
        })
    }

    /// The `SPAWN_RESOURCE` frame `phux spawn` builds for the same flags.
    fn frame(&self) -> FrameKind {
        let durable = self.retain_secs.is_some() || self.idempotency_key.is_some();
        FrameKind::SpawnResource {
            request_id: 1,
            // v0.1 servers expose the single default group (SPEC §3.1).
            group: GroupId::new(1),
            command: (!self.command.is_empty()).then(|| self.command.clone()),
            cwd: self.cwd.clone(),
            env: None,
            term: None,
            satellite: self.satellite.clone().map(SatelliteHost::new),
            owner_terminal: None,
            agent_session: None,
            // A headless spawn has no viewport; the pane takes the server
            // default and is sized by whichever client attaches.
            initial_size: None,
            resource: durable.then(|| {
                Box::new(
                    SpawnResource::default()
                        .with_retain_secs(self.retain_secs)
                        .with_idempotency_key(self.idempotency_key),
                )
            }),
        }
    }
}

/// The optional `retain_secs` argument, bounded to `0..=86400`.
fn retain_secs(args: &Value) -> Result<Option<u32>, ToolError> {
    args.get("retain_secs").map_or(Ok(None), |value| {
        value
            .as_u64()
            .filter(|secs| *secs <= 86_400)
            .and_then(|secs| u32::try_from(secs).ok())
            .map(Some)
            .ok_or_else(|| ToolError::new("`retain_secs` must be an integer in 0..=86400"))
    })
}

fn idempotency_key(raw: &str) -> Result<IdempotencyKey, ToolError> {
    phux_client::spawn::parse_idempotency_key(raw).map_err(|err| {
        contract_error(
            "invalid_idempotency_key",
            err.to_string(),
            "generate one with `openssl rand -hex 16` and reuse it on every retry of the same \
             request",
            2,
        )
    })
}

/// `phux_spawn` — `SPAWN_RESOURCE` through `phux_client::spawn`, with the
/// ownership-verify and `KILL_RESOURCE` rollback behind explicit placement.
pub(crate) async fn spawn(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &[
            "target",
            "satellite",
            "split",
            "ratio",
            "projection",
            "cwd",
            "command",
            "retain_secs",
            "idempotency_key",
            "socket",
        ],
        &[],
    )?;
    let request = SpawnRequest::parse(args)?;
    let socket = socket_arg(args)?;
    let frame = request.frame();
    refuse_unsupported(&socket, &request, &frame).await?;
    let result = match request.target.as_deref() {
        Some(target) => spawn_placed(&socket, frame, target, &request).await?,
        None => {
            phux_client::spawn::spawn_on(&socket, &frame)
                .await
                .map_err(|err| transport_error(&err, &socket, "spawn"))?
                .0
        }
    };
    spawn_document(result)
}

/// Refuse a durable spawn the server would silently downgrade: it does not
/// advertise the feature `retain_secs` or `idempotency_key` needs. A plain
/// spawn costs no extra connection.
async fn refuse_unsupported(
    socket: &Path,
    request: &SpawnRequest,
    frame: &FrameKind,
) -> Result<(), ToolError> {
    if request.retain_secs.is_none() && request.idempotency_key.is_none() {
        return Ok(());
    }
    let features = phux_client::spawn::server_features(socket)
        .await
        .map_err(|err| transport_error(&err, socket, "spawn"))?;
    let Some(missing) = phux_client::spawn::missing_spawn_feature(frame, features) else {
        return Ok(());
    };
    let (flag, name) = match missing {
        ServerFeature::RetainOnExit => ("--retain", "retain_on_exit"),
        _ => ("--idempotency-key", "spawn_idempotency"),
    };
    Err(contract_error(
        "unsupported_server",
        format!("this server does not support {flag}: it does not advertise {name}"),
        "upgrade the server (`phux upgrade` after installing a newer phux), or drop the flag",
        2,
    ))
}

/// Resolve an explicit local owner, spawn into its exact window, then
/// splice the new leaf into the shared layout beside it. A placement that
/// fails after the spawn rolls the new Terminal back.
async fn spawn_placed(
    socket: &Path,
    mut frame: FrameKind,
    target: &str,
    request: &SpawnRequest,
) -> Result<SpawnResult, ToolError> {
    let selector = parse_selector(target)?;
    let (snapshot, _) = state::get_state(socket)
        .await
        .map_err(|err| transport_error(&err, socket, "spawn"))?
        .into_parts();
    let (owner, owner_window, owner_session) =
        placement_owner(socket, &selector, &snapshot).await?;
    if let FrameKind::SpawnResource { owner_terminal, .. } = &mut frame {
        *owner_terminal = Some(owner.clone());
    }
    let (spawned, _) = phux_client::spawn::spawn_on(socket, &frame)
        .await
        .map_err(|err| transport_error(&err, socket, "spawn"))?;
    let SpawnResult::Ok(new_pane) = &spawned else {
        return Ok(spawned);
    };
    let placement = Placement {
        owner,
        owner_window,
        owner_session,
        new_pane: new_pane.clone(),
    };
    let mut notices = Vec::new();
    match phux_client::spawn::verify_and_publish_placement(
        socket,
        &placement,
        request.split,
        request.ratio,
        request.projection.as_deref(),
        2,
        &mut notices,
    )
    .await
    {
        RollbackOutcome::Placed => Ok(spawned),
        RollbackOutcome::RolledBack { reason } => Err(ToolError::new(format!(
            "spawn placement failed; spawned pane was removed: {reason}"
        ))),
        RollbackOutcome::RollbackUnconfirmed {
            reason,
            cleanup_note,
        } => Err(ToolError::new(format!(
            "spawn placement failed ({reason}); {cleanup_note}"
        ))),
    }
}

/// The local Terminal `selector` names, with its window and session.
async fn placement_owner(
    socket: &Path,
    selector: &Selector,
    snapshot: &SessionSnapshot,
) -> Result<
    (
        ResourceId,
        phux_protocol::ids::WindowId,
        phux_protocol::ids::SessionId,
    ),
    ToolError,
> {
    let candidates = state::resolve_targets(socket, selector, snapshot).await;
    let owner = selector::pick_target_pane(&candidates, &snapshot.focused_resource)
        .ok_or_else(|| ToolError::new("no such target"))?;
    if !matches!(owner, ResourceId::Local { .. }) {
        return Err(ToolError::new("explicit spawn placement is local-only"));
    }
    let (window, session) = phux_client::spawn::ownership_for_terminal(snapshot, &owner)
        .ok_or_else(|| ToolError::new("target has no local session ownership"))?;
    Ok((owner, window, session))
}

fn spawn_document(result: SpawnResult) -> Result<Value, ToolError> {
    match result {
        SpawnResult::Ok(id) => Ok(phux_client::spawn::spawned_document(&id, false)),
        // A keyed retry inside the server's horizon: the first spawn's pane,
        // already placed by that spawn, so nothing is placed again.
        SpawnResult::Replayed { id, .. } => Ok(phux_client::spawn::spawned_document(&id, true)),
        SpawnResult::Err(SpawnError::IdempotencyConflict) => Err(contract_error(
            "idempotency_conflict",
            "the idempotency key was already used for a different spawn; nothing was spawned",
            "reuse a key only to retry the identical spawn; draw a fresh key for a new one",
            2,
        )),
        SpawnResult::Err(err) => Err(ToolError::new(phux_client::spawn::spawn_error_message(
            &err,
        ))),
        // `SpawnResult` is `#[non_exhaustive]`: version skew.
        _ => Err(ToolError::new(phux_client::explain::unexpected_reply(
            "SPAWN_RESOURCE",
        ))),
    }
}

// -----------------------------------------------------------------------------
// signal
// -----------------------------------------------------------------------------

/// `phux_signal` — one `SIGNAL_TERMINAL` to the resolved pane's process
/// group (ADR-0033), resolved as an input verb resolves.
pub(crate) async fn signal(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "signal", "confirm", "socket"],
        &["target", "signal"],
    )?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let signal = enum_string(
        args,
        "signal",
        &["interrupt", "freeze", "resume", "terminate", "kill"],
        None,
    )?;
    let confirm = optional_bool(args, "confirm")?.unwrap_or(false);
    // L17 replaces this hand-written confirm with one table-driven check.
    if matches!(signal.as_str(), "interrupt" | "terminate" | "kill") && !confirm {
        return Err(ToolError::new(format!(
            "signal {signal:?} is destructive; pass `confirm: true`"
        )));
    }
    let socket = socket_arg(args)?;
    let selector = parse_selector(&target)?;
    let view = state::get_state(&socket).await?;
    let terminal = resolve_one_for_input(&socket, &selector, &view).await?;
    let mut conn = Connection::connect(&socket).await?;
    let command = phux_client::signal::signal_command(terminal, wire_signal(&signal));
    let (result, _) = conn.request(1, command).await?.into_parts();
    drop(conn);
    match LeaseOutcome::from_result(result) {
        LeaseOutcome::Ok => Ok(json!({
            "schema_version": 1,
            "signaled": true,
            "target": target,
            "signal": signal,
        })),
        LeaseOutcome::Refused(message) => Err(ToolError::new(format!(
            "signal refused for {target}: {message}"
        ))),
        LeaseOutcome::Unexpected(other) => Err(ToolError::new(format!(
            "{target}: {}",
            phux_client::explain::explain_unexpected("signal", &other)
        ))),
    }
}

fn wire_signal(name: &str) -> TerminalSignal {
    match name {
        "interrupt" => TerminalSignal::Interrupt,
        "freeze" => TerminalSignal::Freeze,
        "resume" => TerminalSignal::Resume,
        "terminate" => TerminalSignal::Terminate,
        _ => TerminalSignal::Kill,
    }
}

// -----------------------------------------------------------------------------
// tag
// -----------------------------------------------------------------------------

/// `phux_tag` — read or edit Terminal tags through `phux_client::tags`, the
/// home `phux tag` calls. Edits are read back from the server, never echoed.
pub(crate) async fn tag(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["action", "target", "tags", "socket"],
        &["action", "target"],
    )?;
    let action = enum_string(args, "action", &["ls", "add", "rm"], None)?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let tags = bounded_strings(args, "tags", false)?;
    if action == "ls" && !tags.is_empty() {
        return Err(ToolError::new("`tags` is not accepted for action `ls`"));
    }
    if action != "ls" && tags.is_empty() {
        return Err(ToolError::new("`tags` is required for add/rm"));
    }
    let socket = socket_arg(args)?;
    let selector = parse_selector(&target)?;
    let mut session = phux_client::tags::prepare(&socket, &selector).await?;
    if session.targets.is_empty() {
        return Err(crate::kill_tool::target_miss(&target, &session.degradation));
    }
    let rows = match action.as_str() {
        "ls" => listed_rows(&session),
        edit => edited_rows(&mut session, edit, &tags).await?,
    };
    drop(session);
    let terminals: Vec<Value> = rows
        .iter()
        .map(|(id, tags)| json!({ "terminal": format_terminal_id(id), "tags": tags }))
        .collect();
    Ok(json!({ "schema_version": 1, "action": action, "terminals": terminals }))
}

/// Each resolved Terminal with its current tags, from the fetched index.
fn listed_rows(session: &TagSession) -> Vec<(ResourceId, Vec<String>)> {
    session
        .targets
        .iter()
        .map(|id| {
            (
                id.clone(),
                session.index.get(id).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

/// Apply `add`/`rm` to every resolved Terminal, returning the confirmed
/// tags. Request ids and write order match `phux tag`'s.
async fn edited_rows(
    session: &mut TagSession,
    action: &str,
    tags: &[String],
) -> Result<Vec<(ResourceId, Vec<String>)>, ToolError> {
    let wanted = phux_client::tags::normalize(tags);
    let mut rows = Vec::with_capacity(session.targets.len());
    let mut request_id: u32 = 100;
    for id in session.targets.clone() {
        let mut current = session.index.get(&id).cloned().unwrap_or_default();
        edit_tags(&mut current, action, &wanted);
        request_id += 1;
        let (outcome, _) =
            phux_client::tags::write_tags(&mut session.conn, request_id, &id, &current).await?;
        request_id += 1;
        match outcome {
            TagWriteOutcome::Confirmed(confirmed) => rows.push((id, confirmed)),
            TagWriteOutcome::Refused(refusal) => {
                return Err(ToolError::new(format!(
                    "tag write to {} could not be confirmed: server refused the read: {refusal}",
                    format_terminal_id(&id),
                )));
            }
        }
    }
    Ok(rows)
}

/// `add` appends each missing tag; `rm` drops each named one. The result is
/// sorted and de-duplicated, as `phux tag` writes it.
fn edit_tags(current: &mut Vec<String>, action: &str, tags: &[String]) {
    if action == "add" {
        for tag in tags {
            if !current.iter().any(|existing| existing == tag) {
                current.push(tag.clone());
            }
        }
    } else {
        current.retain(|existing| !tags.iter().any(|tag| tag == existing));
    }
    current.sort();
    current.dedup();
}

// -----------------------------------------------------------------------------
// rename
// -----------------------------------------------------------------------------

/// `phux_rename` — the `phux.session.name/v1` write through
/// `phux_client::session::rename`, checked against a fresh snapshot first
/// (`SET_METADATA` has no reply to refuse on) and followed by the
/// `GET_STATE` ordering barrier `phux rename` sends.
pub(crate) async fn rename(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["session", "new_name", "socket"],
        &["session", "new_name"],
    )?;
    let session = bounded_string(args, "session", true)?.unwrap_or_default();
    let new_name = bounded_string(args, "new_name", true)?.unwrap_or_default();
    let socket = socket_arg(args)?;
    let mut conn = Connection::connect(&socket).await?;
    let renamed = rename_on(&mut conn, &session, &new_name).await;
    if renamed.is_ok() {
        conn.shutdown().await;
    } else {
        drop(conn);
    }
    renamed?;
    Ok(json!({
        "schema_version": 1,
        "renamed": { "from": session, "to": new_name },
    }))
}

/// The checked rename on an open connection: refuse against a fresh
/// snapshot, write, then the ordering barrier.
async fn rename_on(conn: &mut Connection, session: &str, new_name: &str) -> Result<(), ToolError> {
    let (snapshot, _) = state::get_state_on(conn).await?.into_parts();
    if let Some(reason) = rename_refusal(&snapshot, session, new_name) {
        return Err(ToolError::new(format!(
            "rename refused for session {session:?}: {reason}"
        )));
    }
    phux_client::session::rename(conn, 1, session, new_name).await?;
    // An ordering barrier, not a verdict: once the server answers this
    // GET_STATE it has processed the write before it.
    let _ordering_barrier = state::get_state_on(conn).await?;
    Ok(())
}

/// Why the rename must not be sent: an unknown session, or a new name
/// another session already holds. Session names are hub-local, so a partial
/// fleet cannot hide either.
fn rename_refusal(snapshot: &SessionSnapshot, session: &str, new_name: &str) -> Option<String> {
    let has = |name: &str| snapshot.sessions.iter().any(|s| s.name == name);
    if !has(session) {
        return Some("no such session".to_owned());
    }
    if session != new_name && has(new_name) {
        return Some(format!("{new_name:?} already exists"));
    }
    None
}

// -----------------------------------------------------------------------------
// insert / move / swap
// -----------------------------------------------------------------------------

/// Which spatial edit a tool performs.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Spatial {
    Insert,
    Move,
    Swap,
}

impl Spatial {
    const fn roles(self) -> [&'static str; 2] {
        match self {
            Self::Insert => ["target", "new_pane"],
            Self::Move => ["source", "target"],
            Self::Swap => ["first", "second"],
        }
    }

    const fn geometry(self) -> bool {
        !matches!(self, Self::Swap)
    }
}

/// `phux_insert_pane` / `phux_move_pane` / `phux_swap_pane` — through
/// [`phux_client::spatial::run`], the implementation the CLI verbs call.
/// A refusal carries the same stable code the CLI's `--json` error line does.
pub(crate) async fn spatial(args: &Value, edit: Spatial) -> Result<Value, ToolError> {
    let roles = edit.roles();
    let mut allowed = roles.to_vec();
    allowed.extend(["socket", "projection"]);
    if edit.geometry() {
        allowed.extend(["direction", "ratio"]);
    }
    strict_object(args, &allowed, &roles)?;
    let [first, second] = [
        bounded_string(args, roles[0], true)?.unwrap_or_default(),
        bounded_string(args, roles[1], true)?.unwrap_or_default(),
    ];
    let direction = match enum_string(
        args,
        "direction",
        &["horizontal", "vertical"],
        Some("horizontal"),
    )?
    .as_str()
    {
        "vertical" => Direction::Vertical,
        _ => Direction::Horizontal,
    };
    let ratio = cli_ratio(args)?;
    // ADR-0129: 0 or 1 key for a same-session edit; a cross-session
    // `move-pane` needs both (source and destination).
    let projection = bounded_strings(args, "projection", false)?;
    let socket = socket_arg(args)?;
    let operation = match edit {
        Spatial::Insert => SpatialOp::Insert {
            target: first,
            new_pane: second,
            direction,
            ratio,
            projection,
        },
        Spatial::Move => SpatialOp::Move {
            source: first,
            target: second,
            direction,
            ratio,
            projection,
        },
        Spatial::Swap => SpatialOp::Swap {
            first,
            second,
            projection,
        },
    };
    let mut notices = Vec::new();
    match phux_client::spatial::run(&socket, operation, &mut notices).await {
        Ok(outcome) => Ok(outcome.document),
        Err(SpatialError::Transport(err)) => Err(transport_error(&err, &socket, "layout")),
        Err(SpatialError::Refused(refusal)) => Err(contract_error(
            refusal.code,
            refusal.message,
            refusal.remedy,
            refusal.exit_code,
        )),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    /// Validation happens before any connection: a socket that cannot exist
    /// proves nothing was dialed.
    #[tokio::test]
    async fn malformed_or_dangerous_calls_are_refused_before_any_connection() {
        let socket = "/nonexistent/phux-mcp-pane-tools.sock";
        let refused = [
            signal(&json!({ "target": "@1", "signal": "kill", "socket": socket })).await,
            spawn(&json!({ "target": "@1", "satellite": "edge", "socket": socket })).await,
            spawn(&json!({ "split": "vertical", "socket": socket })).await,
            spawn(&json!({ "retain_secs": 86_401, "socket": socket })).await,
            tag(&json!({ "action": "ls", "target": "@1", "tags": ["x"], "socket": socket })).await,
            tag(&json!({ "action": "add", "target": "@1", "socket": socket })).await,
            spatial(
                &json!({ "source": "@1", "target": "@2", "extra": true, "socket": socket }),
                Spatial::Move,
            )
            .await,
            spatial(
                &json!({ "first": "@1", "second": "@2", "direction": "vertical" }),
                Spatial::Swap,
            )
            .await,
        ];
        for result in refused {
            let err = result.expect_err("refused");
            assert!(
                !err.0.contains("no server running"),
                "validation must refuse before dialing: {}",
                err.0
            );
        }
    }

    /// A malformed idempotency key is the `--json` usage refusal, before any
    /// connection.
    #[tokio::test]
    async fn an_all_zero_idempotency_key_is_a_contract_refusal() {
        let err = spawn(&json!({
            "idempotency_key": "00000000000000000000000000000000",
            "socket": "/nonexistent/phux.sock",
        }))
        .await
        .unwrap_err();
        let document: Value = serde_json::from_str(&err.0).expect("a JSON error line");
        assert_eq!(document["error"]["code"], "invalid_idempotency_key");
        assert_eq!(document["exit_code"], 2);
    }

    #[test]
    fn cli_ratio_reads_the_argument_as_the_cli_parsed_it() {
        assert!((cli_ratio(&json!({})).unwrap() - 0.5).abs() < f32::EPSILON);
        assert_eq!(
            cli_ratio(&json!({ "ratio": 0.3 })).unwrap().to_bits(),
            "0.3".parse::<f32>().unwrap().to_bits()
        );
        assert!(cli_ratio(&json!({ "ratio": 1.0 })).is_err());
    }

    #[test]
    fn tag_edits_sort_dedup_and_remove() {
        let mut tags = vec!["b".to_owned(), "a".to_owned()];
        edit_tags(&mut tags, "add", &["c".to_owned(), "a".to_owned()]);
        assert_eq!(tags, ["a", "b", "c"]);
        edit_tags(&mut tags, "rm", &["b".to_owned()]);
        assert_eq!(tags, ["a", "c"]);
    }
}
