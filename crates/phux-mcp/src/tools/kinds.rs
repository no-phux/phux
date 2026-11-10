// -----------------------------------------------------------------------------
// The Terminal tools' kind check (`phux_client::read`).
// -----------------------------------------------------------------------------

/// A target as a Terminal tool resolved it. An exact ID is taken as given,
/// with no inventory read, so its kind stays unknown unless the server
/// refuses it; any other selector read the inventory, which names the kind.
pub(crate) struct Target {
    id: ResourceId,
    kind: Option<phux_protocol::ids::ResourceKind>,
}

/// Resolve `selector` as [`resolve_selector`] does, keeping the kind the
/// inventory read reports. An exact ID costs no round trip.
///
/// # Errors
///
/// As [`resolve_selector`].
pub(crate) async fn resolve_kinded(
    socket: &Path,
    selector: &Selector,
    for_input: bool,
) -> Result<Target, ToolError> {
    if let Some(id) = selector.explicit_id() {
        return Ok(Target { id, kind: None });
    }
    let view = state::get_state(socket).await?;
    let id = resolve_one_with(socket, selector, &view, for_input).await?;
    let kind = Resolved::in_snapshot(view.snapshot(), id.clone()).kind;
    Ok(Target {
        id,
        kind: Some(kind),
    })
}

impl Target {
    /// Refuse a target already known not to be a Terminal.
    ///
    /// # Errors
    ///
    /// `wrong_resource_kind` naming what to use instead.
    fn require_terminal(&self, tool: &str, verb: Verb) -> Result<(), ToolError> {
        let Some(kind) = self.kind else {
            return Ok(());
        };
        let resolved = Resolved {
            id: self.id.clone(),
            kind,
        };
        kind_refusal(tool, verb, &resolved).map_or(Ok(()), Err)
    }

    /// The tool error for `err`, a failed Terminal command on this target.
    /// A wrong-kind refusal of an exact ID reads the kind now, to name what
    /// to use instead; any other failure stands as it was.
    async fn refused(&self, tool: &str, verb: Verb, socket: &Path, err: AttachError) -> ToolError {
        if self.kind.is_some() || !matches!(err, AttachError::WrongResourceKind(_)) {
            return err.into();
        }
        let Ok(view) = state::get_state(socket).await else {
            return err.into();
        };
        let resolved = Resolved::in_snapshot(view.snapshot(), self.id.clone());
        kind_refusal(tool, verb, &resolved).unwrap_or_else(|| err.into())
    }
}

/// The refusal for a target `tool` does not reach, or `None` for a
/// Terminal. In MCP the screen readers read a Terminal only: a document
/// read attaches, which would cost them their read-only annotation, so they
/// name the tool that reads it instead.
fn kind_refusal(tool: &str, verb: Verb, resolved: &Resolved) -> Option<ToolError> {
    if matches!(read::route(verb, resolved), Ok(Route::Terminal)) {
        return None;
    }
    Some(contract_error(
        "wrong_resource_kind",
        format!(
            "{tool} reaches a terminal only, and {} is {}",
            selector::format_terminal_id(&resolved.id),
            read::kind_label(resolved.kind)
        ),
        &instead(verb, resolved),
        2,
    ))
}

/// What reads or drives `resolved` in place of `verb`'s Terminal tool.
fn instead(verb: Verb, resolved: &Resolved) -> String {
    use phux_protocol::ids::ResourceKind;
    let id = selector::format_terminal_id(&resolved.id);
    match (verb, resolved.kind) {
        (Verb::Snapshot, ResourceKind::AccessibilityTree) => "use phux_tree_snapshot".to_owned(),
        (Verb::Snapshot, ResourceKind::PixelSurface) => "use phux_surface_snapshot".to_owned(),
        (Verb::Wait, ResourceKind::AccessibilityTree) => "use phux_tree_wait".to_owned(),
        (Verb::Wait, ResourceKind::PixelSurface) => {
            "a surface is pixels, with no text to wait on; read it with phux_surface_snapshot"
                .to_owned()
        }
        (Verb::Snapshot, ResourceKind::Document) => {
            format!("run `phux snapshot {id}`; no MCP tool reads a typed document's content yet")
        }
        (Verb::Wait, ResourceKind::Document) => format!(
            "run `phux wait {id} --until TEXT`; no MCP tool waits on a typed document yet"
        ),
        (Verb::SendKeys, ResourceKind::AccessibilityTree | ResourceKind::PixelSurface) => {
            "send its producer typed intents with phux_document_type or phux_document_key"
                .to_owned()
        }
        (Verb::SendKeys, ResourceKind::Document) => {
            "a typed document takes no keys: its producer publishes it".to_owned()
        }
        (Verb::SendKeys, ResourceKind::AgentSession) => {
            "prompt the agent in its parent pane with phux_agent_prompt".to_owned()
        }
        (_, ResourceKind::AgentSession) => "read an agent session with phux_agent_log".to_owned(),
        _ => "this phux does not know the resource's kind; update phux".to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod kind_tests {
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::ids::ResourceKind;
    use phux_protocol::wire::frame::{Command, FrameKind};

    use super::*;

    /// An exact Terminal ID costs no inventory read: one connection, no
    /// `GET_STATE`. A second dial would never be accepted, so the timeout
    /// catches one too.
    #[tokio::test]
    async fn an_exact_terminal_id_reads_no_inventory() {
        let screen = ScreenState {
            pane: 1,
            cols: 9,
            rows: 1,
            lines: vec!["the quick".to_owned()],
            ..ScreenState::default()
        };
        for (tool, mut args) in [
            ("phux_snapshot", json!({ "target": "@1" })),
            ("phux_wait", json!({ "target": "@1", "until": "quick", "timeout_secs": 5 })),
            ("phux_send_keys", json!({ "target": "@1", "keys": ["x"] })),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("exact.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let spec = ScriptSpec::new().screen(&screen);
            let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
            args["socket"] = json!(socket);
            tokio::time::timeout(Duration::from_secs(10), dispatch(tool, &args))
                .await
                .unwrap_or_else(|_| panic!("{tool} dialed twice"))
                .unwrap_or_else(|err| panic!("{tool}: {err}"));
            let frames = server.await.unwrap();
            let read_state = frames.iter().any(|frame| {
                matches!(frame, FrameKind::Command { command: Command::GetState { .. }, .. })
            });
            assert!(!read_state, "{tool} read the inventory");
        }
    }

    /// A kind the inventory already named is refused before any command.
    #[test]
    fn a_known_document_is_refused_before_the_terminal_command() {
        let target = |kind| Target {
            id: ResourceId::local(5),
            kind: Some(kind),
        };
        let tree = target(ResourceKind::AccessibilityTree);
        let err = tree.require_terminal("phux_snapshot", Verb::Snapshot).unwrap_err();
        assert!(err.0.contains("phux_tree_snapshot"), "{err}");
        let session = target(ResourceKind::AgentSession);
        let err = session.require_terminal("phux_wait", Verb::Wait).unwrap_err();
        assert!(err.0.contains("phux_agent_log"), "{err}");
        let pane = target(ResourceKind::Terminal);
        assert!(pane.require_terminal("phux_send_keys", Verb::SendKeys).is_ok());
        let exact = Target {
            id: ResourceId::local(5),
            kind: None,
        };
        assert!(exact.require_terminal("phux_snapshot", Verb::Snapshot).is_ok());
    }
}
