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

/// Exact IDs bypass inventory/name resolution, including for retained resources.
pub(crate) async fn resolve_selector(
    socket: &Path,
    selector: &Selector,
    for_input: bool,
) -> Result<ResourceId, ToolError> {
    if let Some(id) = selector.explicit_id() {
        return Ok(id);
    }
    let view = state::get_state(socket).await?;
    if for_input {
        resolve_one_for_input(socket, selector, &view).await
    } else {
        resolve_one(socket, selector, &view).await
    }
}

/// Resolve `selector` against `view` to one pane as the CLI does (ADR-0021),
/// preferring the focused pane. A miss against a degraded view says so
/// rather than claiming the target is gone. For read tools; a tool that
/// delivers input resolves through [`resolve_one_for_input`].
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

/// [`resolve_one`] for the tools that deliver input into the pane
/// (`phux_send_keys`, `phux_paste`): identical, except that a `%name` whose
/// record has the withdrawn shape is refused (ADR-0075 point 5), as the
/// CLI's input verbs refuse it.
///
/// # Errors
///
/// A [`ToolError`] when the selector matches no pane, or `agent_withdrawn`.
pub(crate) async fn resolve_one_for_input(
    socket: &std::path::Path,
    selector: &Selector,
    view: &StateView,
) -> Result<ResourceId, ToolError> {
    resolve_one_with(socket, selector, view, true).await
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
