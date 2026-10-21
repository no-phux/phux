// --- Subject extraction, per row ---

/// The Terminal a "named Terminal" row names.
pub(super) fn named_terminal(request: Request<'_>) -> Option<&WireResourceId> {
    match request {
        Request::Frame(frame) => frame_terminal(frame),
        Request::Command(command) => command_terminal(command),
        Request::StreamBind(terminal) => Some(terminal),
    }
}

const fn frame_terminal(frame: &FrameKind) -> Option<&WireResourceId> {
    match frame {
        FrameKind::HistoryRequest { terminal_id, .. }
        | FrameKind::FrameAck { terminal_id, .. }
        | FrameKind::InputKey { terminal_id, .. }
        | FrameKind::InputMouse { terminal_id, .. }
        | FrameKind::InputFocus { terminal_id, .. }
        | FrameKind::InputPaste { terminal_id, .. }
        | FrameKind::InputTerminalReply { terminal_id, .. }
        | FrameKind::ResizeTerminal { terminal_id, .. } => Some(terminal_id),
        FrameKind::SubscribeEvents { terminal, .. } => terminal.as_ref(),
        _ => None,
    }
}

fn command_terminal(command: &Command) -> Option<&WireResourceId> {
    match command {
        Command::NativeAgent { request } => match request.request() {
            phux_protocol::native_agent::NativeRequest::Spawn { .. }
            | phux_protocol::native_agent::NativeRequest::ListIntegrations {}
            | phux_protocol::native_agent::NativeRequest::LaunchIntegration { .. } => None,
            phux_protocol::native_agent::NativeRequest::Command {
                agent_session_id, ..
            }
            | phux_protocol::native_agent::NativeRequest::Register {
                agent_session_id, ..
            }
            | phux_protocol::native_agent::NativeRequest::Receive {
                agent_session_id, ..
            }
            | phux_protocol::native_agent::NativeRequest::Complete {
                agent_session_id, ..
            }
            | phux_protocol::native_agent::NativeRequest::Publish {
                agent_session_id, ..
            }
            | phux_protocol::native_agent::NativeRequest::Close {
                agent_session_id, ..
            } => Some(agent_session_id),
        },
        Command::AttachResource { terminal_id, .. }
        | Command::DetachResource { terminal_id }
        | Command::KillResource { terminal_id, .. }
        | Command::KillResourceIf { terminal_id, .. }
        | Command::GetScreen { terminal_id, .. }
        | Command::RouteInput { terminal_id, .. }
        | Command::ApplyInput { terminal_id, .. }
        | Command::GetTerminalState { terminal_id, .. }
        | Command::SubscribeResourceEvents { terminal_id, .. }
        | Command::AcquireInput { terminal_id, .. }
        | Command::ReleaseInput { terminal_id }
        | Command::SignalTerminal { terminal_id, .. }
        | Command::ReportAsked { terminal_id, .. }
        | Command::ReportAgentState { terminal_id, .. }
        | Command::PutFile { terminal_id, .. }
        | Command::Checkout { terminal_id, .. }
        | Command::Transcribe { terminal_id, .. }
        | Command::AppendResourceOutput { terminal_id, .. } => Some(terminal_id),
        Command::ResourceTerminate { target, .. } => Some(&target.resource),
        _ => None,
    }
}

/// A session by wire key as a Group point; `None` when no record has it.
fn session_key_point(s: &ServerState, key: &phux_protocol::ids::SessionKey) -> Option<Point> {
    let id = s
        .registry()
        .session_id(&crate::org_wire::core_session_key(key))?;
    Some(session_point(s, Some(id)))
}

/// A resource by wire key as a Terminal point matched through its current
/// session's Group; `None` when no record has it.
fn resource_key_point(s: &ServerState, key: &phux_protocol::ids::ResourceKey) -> Option<Point> {
    let id = s
        .registry()
        .resource_id(&crate::org_wire::core_resource_key(key))?;
    Some(Point::Terminal(local_point(s, id)))
}

/// The session a keyed session command names.
fn keyed_session(s: &ServerState, request: Request<'_>) -> Option<Point> {
    let Request::Command(command) = request else {
        return None;
    };
    match command {
        Command::NativeAgent { request } => match request.request() {
            phux_protocol::native_agent::NativeRequest::LaunchIntegration { session, .. } => {
                session_key_point(s, session)
            }
            phux_protocol::native_agent::NativeRequest::Spawn { spec } => s
                .find_session_by_name(&spec.session)
                .map(|session| session_point(s, Some(session))),
            _ => None,
        },
        Command::SessionRename { session, .. }
        | Command::SessionSetProject { session, .. }
        | Command::SessionSetLifetime { session, .. }
        | Command::SessionDelete { session, .. }
        | Command::SessionKill { session, .. }
        | Command::InboxResolve { session, .. }
        | Command::ResourceCreate { session, .. } => session_key_point(s, session),
        _ => None,
    }
}

/// The resource a keyed resource command names.
fn keyed_resource(s: &ServerState, request: Request<'_>) -> Option<Point> {
    let Request::Command(command) = request else {
        return None;
    };
    match command {
        Command::ResourceRename { resource, .. }
        | Command::ResourceRemove { resource, .. }
        | Command::ResourceRelaunch { resource, .. } => resource_key_point(s, resource),
        _ => None,
    }
}

/// Every source session a move takes from, each moved resource's current
/// session, and the destination; one unresolvable key refuses the move.
fn move_sessions(s: &ServerState, verbs: Verbs, request: Request<'_>) -> Option<Vec<Need>> {
    let Request::Command(command) = request else {
        return None;
    };
    let mut points = Vec::new();
    match command {
        Command::ResourceMove {
            resource, from, to, ..
        } => {
            points.push(resource_key_point(s, resource)?);
            points.push(session_key_point(s, from)?);
            points.push(session_key_point(s, to)?);
        }
        Command::ResourceMoveMany { items, to, .. } => {
            for item in items {
                points.push(resource_key_point(s, &item.resource)?);
                points.push(session_key_point(s, &item.from)?);
            }
            points.push(session_key_point(s, to)?);
        }
        _ => return None,
    }
    Some(
        points
            .into_iter()
            .map(|point| Need::new(verbs, point))
            .collect(),
    )
}

/// Every session and resource the undone operation's recorded inverse
/// touches, under current membership: for a move, the resource's current
/// session and the session it returns to.
fn undone_operation(s: &ServerState, verbs: Verbs, request: Request<'_>) -> Option<Vec<Need>> {
    let Request::Command(Command::OrganizationUndo { undo, .. }) = request else {
        return None;
    };
    let receipt = s.org().receipt(&crate::org_wire::core_operation(undo))?;
    let Some(inverse) = receipt.undo.as_ref() else {
        return Some(vec![Need::new(verbs, Point::Global)]);
    };
    let session = |key: &phux_core::SessionKey| {
        s.registry()
            .session_id(key)
            .map(|id| session_point(s, Some(id)))
    };
    let mut points = Vec::new();
    for edit in &inverse.inverse {
        match edit {
            // The same rule as a forward move: the resource, the session it
            // leaves now, and the session it returns to.
            phux_core::InverseEdit::Membership { resource, to } => {
                let id = s.registry().resource_id(resource)?;
                points.push(Point::Terminal(local_point(s, id)));
                points.push(session_point(s, Some(s.registry().session_of(id)?)));
                points.push(session(to)?);
            }
            phux_core::InverseEdit::Intake { session: key, .. } => points.push(session(key)?),
            // Restored after the membership, in a session already covered.
            phux_core::InverseEdit::Arrangement { .. } => {}
        }
    }
    Some(
        points
            .into_iter()
            .map(|point| Need::new(verbs, point))
            .collect(),
    )
}

/// The session that owns the arrangement a command names.
fn arrangement_owner(s: &ServerState, request: Request<'_>) -> Option<Point> {
    let Request::Command(command) = request else {
        return None;
    };
    let (Command::ArrangementGet { arrangement: key }
    | Command::ArrangementReplace {
        arrangement: key, ..
    }) = command
    else {
        return None;
    };
    let record = s
        .org()
        .arrangement(&crate::org_wire::core_arrangement_key(key))?;
    let phux_core::ArrangementOwner::Session(owner) = record.owner else {
        return None;
    };
    let id = s.registry().session_id(&owner)?;
    Some(session_point(s, Some(id)))
}

/// `KILL_RESOURCES` / `CLOSE_TAB_RESOURCES`: every named Terminal, all-or-nothing.
/// Zero targets is a no-op, so it needs nothing.
fn every_named(s: &ServerState, verbs: Verbs, request: Request<'_>) -> Vec<Need> {
    let Request::Command(command) = request else {
        return Vec::new();
    };
    super::batch_terminals(command)
        .iter()
        .map(|id| Need::new(verbs, terminal_subject(s, id)))
        .collect()
}

/// `VIEWPORT_RESIZE`: check exactly the subscribed session panes targeted by
/// geometry dispatch. Foreign resource subscriptions are not viewport targets.
fn attached_terminals(s: &ServerState, client: ClientId, verbs: Verbs) -> Vec<Need> {
    s.viewport_terminals(client)
        .into_iter()
        .map(|core| Need::new(verbs, Point::Terminal(local_point(s, core))))
        .collect()
}

fn moved_and_owner(s: &ServerState, verbs: Verbs, request: Request<'_>) -> Option<Vec<Need>> {
    let Request::Frame(FrameKind::MoveResource {
        terminal,
        owner_terminal,
        ..
    }) = request
    else {
        return None;
    };
    Some(vec![
        Need::new(verbs, terminal_subject(s, terminal)),
        Need::new(verbs, terminal_subject(s, owner_terminal)),
    ])
}

/// The Group an `ATTACH` resolves to, or a forced detach names. Absent, or
/// not yet created, is [`Point::LocalHost`].
fn resolved_group(s: &ServerState, request: Request<'_>) -> Option<Point> {
    let session = match request {
        Request::Frame(FrameKind::Attach { target, .. }) => resolve_attach_session(s, target),
        // Clearing keep-empty: the session the value names (L3, ADR-0105).
        Request::Frame(FrameKind::SetMetadata { value, .. }) => {
            decode_session_keep_empty(value).and_then(|(name, _)| s.find_session_by_name(name))
        }
        Request::Command(Command::DetachClients {
            session: Some(name),
        }) => s.find_session_by_name(name),
        _ => return None,
    };
    Some(session_point(s, session))
}

/// Side-effect-free mirror of `resolve_attach_target`; the attach handler
/// pins the id this authorized.
#[must_use]
pub fn resolve_attach_session(
    s: &ServerState,
    target: &AttachTarget,
) -> Option<phux_core::ids::SessionId> {
    match target {
        AttachTarget::Last => last_session(s),
        AttachTarget::ById(wire) => s.idspace.resolve_session(*wire),
        AttachTarget::ByName(name) | AttachTarget::CreateIfMissing { name, .. } => {
            s.find_session_by_name(name)
        }
        _ => None,
    }
}

fn last_session(s: &ServerState) -> Option<phux_core::ids::SessionId> {
    if let Some(session) = s.most_recently_touched_session() {
        return Some(session);
    }
    if s.has_session_touch_history() {
        return None;
    }
    s.pre_seeded_session()
        .and_then(|name| s.find_session_by_name(name))
}

fn session_point(s: &ServerState, session: Option<phux_core::ids::SessionId>) -> Point {
    session
        .and_then(|session| GroupPoint::of(s, session))
        .map_or(Point::LocalHost, Point::Group)
}

fn spawn_satellite(request: Request<'_>) -> Option<String> {
    let Request::Frame(FrameKind::SpawnResource { satellite, .. }) = request else {
        return None;
    };
    satellite.as_ref().map(|host| host.as_str().to_owned())
}

const fn spawn_owner(request: Request<'_>) -> Option<&WireResourceId> {
    let Request::Frame(FrameKind::SpawnResource { owner_terminal, .. }) = request else {
        return None;
    };
    owner_terminal.as_ref()
}

fn spawn_parent(request: Request<'_>) -> Option<&WireResourceId> {
    let Request::Frame(FrameKind::SpawnResource { resource, .. }) = request else {
        return None;
    };
    resource.as_deref().and_then(|spawn| spawn.parent.as_ref())
}

/// `CREATE` on the anchor's Group and `BIND` on the anchor; no local Group
/// fails closed.
fn create_beside(s: &ServerState, anchor: &WireResourceId) -> Option<Vec<Need>> {
    let anchor = TerminalPoint::of(s, anchor);
    let group = anchor.local_group()?;
    Some(vec![
        Need::new(CREATE, Point::Group(group)),
        Need::new(BIND, Point::Terminal(anchor.alone())),
    ])
}

/// `CREATE` on the satellite host and `BIND` on the satellite-tagged parent.
fn satellite_agent_spawn(request: Request<'_>) -> Option<Vec<Need>> {
    let host = spawn_satellite(request)?;
    let WireResourceId::Satellite {
        host: parent_host,
        id,
    } = spawn_parent(request)?
    else {
        return None;
    };
    let parent = TerminalPoint {
        at: Located::Satellite(parent_host.as_str().to_owned(), *id),
        group: None,
        key: None,
        parent: None,
    };
    Some(vec![
        Need::new(CREATE, Point::SatelliteHost(host)),
        Need::new(BIND, Point::Terminal(parent)),
    ])
}

const fn metadata_scope(request: Request<'_>) -> Option<&Scope> {
    let Request::Frame(frame) = request else {
        return None;
    };
    match frame {
        FrameKind::GetMetadata { scope, .. }
        | FrameKind::SetMetadata { scope, .. }
        | FrameKind::DeleteMetadata { scope, .. }
        | FrameKind::ListMetadata { scope, .. }
        | FrameKind::SubscribeMetadata { scope, .. } => Some(scope),
        _ => None,
    }
}
