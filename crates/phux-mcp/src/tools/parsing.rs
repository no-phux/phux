// -----------------------------------------------------------------------------
// Shared helpers.
// -----------------------------------------------------------------------------

/// Enforce a strict object schema at runtime (JSON Schema is advisory at
/// the MCP boundary): an object, no unknown keys, every required key.
pub(crate) fn strict_object(
    args: &Value,
    allowed: &[&str],
    required: &[&str],
) -> Result<(), ToolError> {
    let object = args
        .as_object()
        .ok_or_else(|| ToolError::new("tool arguments must be an object"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ToolError::new(format!("unknown argument `{key}`")));
    }
    if let Some(key) = required.iter().find(|key| !object.contains_key(**key)) {
        return Err(ToolError::new(format!("missing required argument `{key}`")));
    }
    Ok(())
}

/// Parse a selector, naming the bad input in the error.
pub(crate) fn parse_selector(raw: &str) -> Result<Selector, ToolError> {
    selector::parse(raw).map_err(|err| ToolError::new(format!("invalid target '{raw}': {err}")))
}

/// The optional `target` as a [`Selector`], the focused session when absent.
fn parse_target(args: &Value) -> Result<Selector, ToolError> {
    str_arg(args, "target").map_or(Ok(Selector::Current), parse_selector)
}

/// The required `target` as a [`Selector`].
fn required_target(args: &Value) -> Result<Selector, ToolError> {
    parse_selector(required_str(args, "target")?)
}

/// Resolve `selector` to one resource and the kind the inventory lists it
/// as, so a tool can dispatch on it ([`phux_client::read`]). An exact ID,
/// retained resources included, resolves without the inventory; its kind
/// is read from it when it can be, and is otherwise taken as a Terminal,
/// whose read the server answers as before. `for_input` refuses a `%name`
/// whose record has the withdrawn shape (ADR-0075 point 5), as the CLI's
/// input verbs refuse it.
///
/// # Errors
///
/// A [`ToolError`] when the selector matches no pane, or `agent_withdrawn`.
pub(crate) async fn resolve_resource(
    socket: &Path,
    selector: &Selector,
    for_input: bool,
) -> Result<Resolved, ToolError> {
    if let Some(id) = selector.explicit_id() {
        return Ok(match state::get_state(socket).await {
            Ok(view) => Resolved::in_snapshot(view.snapshot(), id),
            Err(_) => Resolved {
                id,
                kind: phux_protocol::ids::ResourceKind::Terminal,
            },
        });
    }
    let view = state::get_state(socket).await?;
    let id = resolve_one_with(socket, selector, &view, for_input).await?;
    Ok(Resolved::in_snapshot(view.snapshot(), id))
}

/// A verb refused for its target's kind, on the CLI's error contract.
pub(crate) fn wrong_kind_error(err: &read::WrongKind) -> ToolError {
    contract_error("wrong_resource_kind", err.to_string(), err.remedy(), 2)
}

/// Refuse anything but a Terminal for `tool`, an MCP screen reader.
///
/// The CLI verb reads a document by kind, through an attach; the MCP tool
/// does not, so that it keeps its read-only annotation, and names the
/// per-kind tool that reads the document instead.
///
/// # Errors
///
/// `wrong_resource_kind` naming the tool to use, or the kind refusal.
fn terminal_reader(tool: &str, verb: Verb, resolved: &Resolved) -> Result<(), ToolError> {
    let routed = read::route(verb, resolved);
    if matches!(routed, Ok(Route::Terminal)) {
        return Ok(());
    }
    let Some(instead) = per_kind_tool(verb, resolved.kind) else {
        return routed.map(drop).map_err(|err| wrong_kind_error(&err));
    };
    Err(contract_error(
        "wrong_resource_kind",
        format!(
            "{tool} reads a terminal's screen, and {} is {}",
            selector::format_terminal_id(&resolved.id),
            read::kind_label(resolved.kind)
        ),
        &format!("use {instead}"),
        2,
    ))
}

/// The tool that reads a document of `kind` for `verb`; `None` for a kind
/// that is not a document.
const fn per_kind_tool(verb: Verb, kind: phux_protocol::ids::ResourceKind) -> Option<&'static str> {
    use phux_protocol::ids::ResourceKind;
    match (verb, kind) {
        (Verb::Snapshot, ResourceKind::AccessibilityTree) => Some("phux_tree_snapshot"),
        (_, ResourceKind::PixelSurface) => Some("phux_surface_snapshot"),
        (_, ResourceKind::AccessibilityTree) | (Verb::Wait, ResourceKind::Document) => {
            Some("phux_tree_wait, which matches any document's text")
        }
        (_, ResourceKind::Document) => Some(
            "`phux snapshot @N` on the CLI; no MCP tool reads a typed document's content yet",
        ),
        _ => None,
    }
}

/// Resolve `selector` against `view` to one pane as the CLI does (ADR-0021),
/// preferring the focused pane. A miss against a degraded view says so
/// rather than claiming the target is gone. For read tools; a tool that
/// delivers input resolves through [`resolve_resource`].
///
/// # Errors
///
/// A [`ToolError`] when the selector matches no pane.
pub(crate) async fn resolve_one(
    socket: &std::path::Path,
    selector: &Selector,
    view: &StateView,
) -> Result<ResourceId, ToolError> {
    resolve_one_with(socket, selector, view, false).await
}

async fn resolve_one_with(
    socket: &std::path::Path,
    selector: &Selector,
    view: &StateView,
    for_input: bool,
) -> Result<ResourceId, ToolError> {
    let snapshot = view.snapshot();
    // `%name` resolves to exactly one agent or refuses (ADR-0075 point 3).
    if let Selector::Agent(name) = selector {
        return state::resolve_agent_target(socket, name, snapshot, for_input)
            .await
            .map(|target| target.terminal)
            .map_err(|err| agent_resolve_error(&err));
    }
    let candidates = state::resolve_targets(socket, selector, snapshot)
        .await
        .map_err(|err| agent_resolve_error(&err))?;
    selector::pick_target_pane(&candidates, &snapshot.focused_resource).ok_or_else(|| {
        if view.is_complete() {
            ToolError::new("no such target")
        } else {
            incomplete_view_miss("the target", view.degradation().notices())
        }
    })
}

/// The explicit socket path, else `$PHUX_SOCKET`, else the daemon default.
fn socket_path(explicit: Option<&str>) -> PathBuf {
    explicit.map_or_else(phux_config::socket::default_socket_path, PathBuf::from)
}

/// The `socket` argument if it is a string, else the default.
pub(crate) fn socket_or_default(args: &Value) -> PathBuf {
    socket_path(str_arg(args, "socket"))
}

/// The `socket` argument, which must be a string when present.
///
/// # Errors
///
/// A present `socket` that is not a string.
pub(crate) fn socket_arg(args: &Value) -> Result<PathBuf, ToolError> {
    match args.get("socket") {
        None => Ok(socket_path(None)),
        Some(Value::String(path)) => Ok(socket_path(Some(path))),
        Some(_) => Err(ToolError::new("`socket` must be a string")),
    }
}

/// A JSON rendering of a `ResourceId` using the canonical direct selector.
fn pane_value(id: &ResourceId) -> Value {
    json!(selector::format_terminal_id(id))
}

/// Read an optional string argument from a tool's params object.
pub(crate) fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// Read a required string argument.
pub(crate) fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    str_arg(args, key).ok_or_else(|| ToolError::new(format!("missing required string `{key}`")))
}

/// Read an optional non-negative integer; anything else is absent.
pub(crate) fn num_arg(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

/// Read an optional `u32`; anything else (including overflow) is absent.
fn u32_arg(args: &Value, key: &str) -> Option<u32> {
    num_arg(args, key).and_then(|n| u32::try_from(n).ok())
}

/// Read an optional boolean; a non-boolean is absent.
fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

/// Read a required array-of-strings argument.
fn string_array(args: &Value, key: &str) -> Result<Vec<String>, ToolError> {
    optional_string_array(args, key)?
        .ok_or_else(|| ToolError::new(format!("missing required array `{key}`")))
}

/// Read an optional array-of-strings argument.
pub(crate) fn optional_string_array(
    args: &Value,
    key: &str,
) -> Result<Option<Vec<String>>, ToolError> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let arr = value
        .as_array()
        .ok_or_else(|| ToolError::new(format!("`{key}` must be an array of strings")))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| ToolError::new(format!("`{key}` must contain only strings")))
        })
        .collect::<Result<Vec<String>, _>>()
        .map(Some)
}
