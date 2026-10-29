// -----------------------------------------------------------------------------
// Classifiers.
// -----------------------------------------------------------------------------

/// Classify one decoded client frame (workload-auth §6).
///
/// A `COMMAND` frame is classified by its nested command; a server-to-client
/// frame arriving from a client is [`Classification::Deny`].
#[must_use]
pub fn classify_frame(frame: &FrameKind) -> Classification {
    frame_rule(frame).classification()
}

/// Classify one decoded nested command (workload-auth §6).
#[must_use]
pub fn classify_command(command: &Command) -> Classification {
    command_rule(command).classification()
}

/// The workload-auth §6 row that classifies `frame`: a `COMMAND` frame's
/// nested command row, default-deny for a server-to-client frame.
#[must_use]
pub fn frame_rule(frame: &FrameKind) -> &'static Rule {
    match frame {
        FrameKind::Hello { .. } => &F_HELLO,
        FrameKind::Ping { .. } => &F_PING,
        FrameKind::Detach => &F_DETACH,
        FrameKind::Attach { target, .. } => attach_rule(target),
        FrameKind::HistoryRequest { .. } => &F_HISTORY_REQUEST,
        FrameKind::FrameAck { .. } => &F_FRAME_ACK,
        FrameKind::Command { command, .. } => command_rule(command),
        FrameKind::InputKey { .. }
        | FrameKind::InputMouse { .. }
        | FrameKind::InputFocus { .. }
        | FrameKind::InputPaste { .. }
        | FrameKind::InputTerminalReply { .. } => &F_INPUT,
        FrameKind::ViewportResize { .. } => &F_VIEWPORT_RESIZE,
        FrameKind::SpawnResource {
            satellite,
            owner_terminal,
            resource,
            ..
        } => spawn_rule(
            satellite.as_ref(),
            owner_terminal.is_some(),
            resource.as_deref(),
        ),
        FrameKind::ResizeTerminal { .. } => &F_RESIZE_TERMINAL,
        FrameKind::MoveResource { .. } => &F_MOVE_RESOURCE,
        FrameKind::SubscribeEvents {
            terminal: Some(_), ..
        } => &F_SUBSCRIBE_EVENTS_ONE,
        FrameKind::SubscribeEvents { terminal: None, .. } => &F_SUBSCRIBE_EVENTS_ALL,
        FrameKind::GetMetadata { scope, key, .. } => get_metadata_rule(scope, key),
        FrameKind::SetMetadata {
            scope, key, value, ..
        } => set_metadata_rule(scope, key, value),
        FrameKind::DeleteMetadata { key, .. } => delete_metadata_rule(key),
        FrameKind::ListMetadata { .. } => &F_LIST_METADATA,
        FrameKind::ListDirectory { .. } => &F_LIST_DIRECTORY,
        FrameKind::PathQuery { .. } => &F_PATH_QUERY,
        FrameKind::SubscribeMetadata { key, .. } => subscribe_metadata_rule(key),
        // Server-to-client frames: wrong direction when a client sends them.
        FrameKind::HelloOk { .. }
        | FrameKind::Pong { .. }
        | FrameKind::ResourceOutput { .. }
        | FrameKind::Attached { .. }
        | FrameKind::AttachReady { .. }
        | FrameKind::Detached { .. }
        | FrameKind::BootstrapBegin { .. }
        | FrameKind::BootstrapChunk { .. }
        | FrameKind::BootstrapReady { .. }
        | FrameKind::HistoryPage { .. }
        | FrameKind::BootstrapTombstone { .. }
        | FrameKind::HistoryTombstone { .. }
        | FrameKind::HistoryRejected { .. }
        | FrameKind::Bell { .. }
        | FrameKind::Error { .. }
        | FrameKind::MetadataChanged { .. }
        | FrameKind::MetadataValue { .. }
        | FrameKind::MetadataKeys { .. }
        | FrameKind::DirectoryListing { .. }
        | FrameKind::PathResults { .. }
        | FrameKind::ResourceSpawned { .. }
        | FrameKind::ResourceMoved { .. }
        | FrameKind::ResourceClosed { .. }
        | FrameKind::CommandResult { .. }
        | FrameKind::Event { .. } => &F_UNCLASSIFIED,
    }
}

/// The workload-auth §6 row that classifies the nested `command`; a command
/// the spec does not list is default-deny.
#[must_use]
pub fn command_rule(command: &Command) -> &'static Rule {
    match command {
        Command::NativeAgent { request } => match request.request() {
            crate::native_agent::NativeRequest::ListIntegrations {} => &C_NATIVE_LIST,
            crate::native_agent::NativeRequest::LaunchIntegration { .. } => &C_NATIVE_LAUNCH,
            crate::native_agent::NativeRequest::Spawn { .. } => &C_NATIVE_SPAWN,
            crate::native_agent::NativeRequest::Command { .. } => &C_NATIVE_COMMAND,
            crate::native_agent::NativeRequest::Register { .. }
            | crate::native_agent::NativeRequest::Receive { .. }
            | crate::native_agent::NativeRequest::Complete { .. }
            | crate::native_agent::NativeRequest::Publish { .. }
            | crate::native_agent::NativeRequest::Close { .. } => &C_NATIVE_CONTROLLER,
        },
        Command::AttachResource { .. } => &C_ATTACH_RESOURCE,
        Command::DetachResource { .. } => &C_DETACH_RESOURCE,
        Command::KillResource { .. } => &C_KILL_RESOURCE,
        Command::KillResourceIf { .. } => &C_KILL_RESOURCE_IF,
        Command::GetScreen { .. } => &C_GET_SCREEN,
        Command::RouteInput { .. } | Command::ApplyInput { .. } => &C_INPUT,
        Command::KillResources { .. } => &C_KILL_RESOURCES,
        Command::CloseTabResources { .. } => &C_CLOSE_TAB_RESOURCES,
        Command::GetState {
            scope: StateScope::Server,
        } => &C_GET_STATE,
        Command::GetTerminalState { .. } => &C_GET_TERMINAL_STATE,
        Command::SubscribeResourceEvents { .. } => &C_SUBSCRIBE_RESOURCE_EVENTS,
        Command::Upgrade => &C_UPGRADE,
        Command::AcquireInput { .. } | Command::ReleaseInput { .. } => &C_INPUT_LEASE,
        Command::SignalTerminal { .. } => &C_SIGNAL_TERMINAL,
        Command::ReportAsked { .. } | Command::ReportAgentState { .. } => &C_AGENT_REPORT,
        Command::PutFile { .. } => &C_PUT_FILE,
        Command::GetFile { .. } => &C_GET_FILE,
        Command::Checkout { .. } => &C_CHECKOUT,
        Command::ServiceHttpOpen { .. }
        | Command::ServiceHttpRead { .. }
        | Command::ServiceHttpClose { .. } => &C_SERVICE_HTTP,
        Command::DetachClients { session: Some(_) } => &C_DETACH_CLIENTS_SESSION,
        Command::DetachClients { session: None } => &C_DETACH_CLIENTS_ALL,
        Command::Shutdown => &C_SHUTDOWN,
        Command::OpenListener { .. } => &C_OPEN_LISTENER,
        Command::GetPerf { reset: false } => &C_GET_PERF,
        Command::GetPerf { reset: true } => &C_GET_PERF_RESET,
        Command::PerfControl { .. } => &C_PERF_CONTROL,
        Command::AppendResourceOutput { .. } => &C_APPEND_RESOURCE_OUTPUT,
        Command::Transcribe { .. } => &C_TRANSCRIBE,
        Command::GetOrganization { .. } => &C_GET_ORGANIZATION,
        Command::DescribeApi => &C_API_DESCRIBE,
        Command::GetCreation { .. } => &C_GET_CREATION,
        Command::SessionCreate { .. } => &C_SESSION_CREATE,
        Command::SessionRename { .. }
        | Command::SessionSetProject { .. }
        | Command::SessionSetLifetime { .. }
        | Command::InboxResolve { .. } => &C_SESSION_ORGANIZE,
        Command::SessionDelete { .. } | Command::SessionKill { .. } => &C_SESSION_DELETE,
        Command::ResourceCreate { placement: Some(_), .. } => &C_RESOURCE_CREATE_PLACED,
        Command::ResourceCreate { .. } => &C_RESOURCE_CREATE,
        Command::ResourceMove { .. } | Command::ResourceMoveMany { .. } => &C_RESOURCE_MOVE,
        Command::ResourceRename { .. } => &C_RESOURCE_RENAME,
        Command::ResourceTerminate { .. } => &C_RESOURCE_TERMINATE,
        Command::ResourceRemove { .. } => &C_RESOURCE_REMOVE,
        Command::ResourceRelaunch { .. } => &C_RESOURCE_RELAUNCH,
        Command::OrganizationUndo { .. } => &C_ORGANIZATION_UNDO,
        Command::ArrangementGet { .. } => &C_ARRANGEMENT_GET,
        Command::ArrangementReplace { .. } => &C_ARRANGEMENT_REPLACE,
        Command::DelegateAgent { .. } | Command::RevokeDelegation { .. } => &C_DELEGATION,
    }
}

fn attach_rule(target: &AttachTarget) -> &'static Rule {
    match target {
        AttachTarget::CreateIfMissing { .. } => &F_ATTACH_CREATE,
        AttachTarget::Last | AttachTarget::ByName(_) | AttachTarget::ById(_) => &F_ATTACH,
    }
}

/// The spawn rows. A kind that contradicts its binding is refused first, so
/// the decoder's `SPAWN_FAILED` never runs; a Terminal is then placed by its
/// satellite and owner fields, and an agent session by its parent.
fn spawn_rule(
    satellite: Option<&SatelliteHost>,
    owned: bool,
    resource: Option<&SpawnResource>,
) -> &'static Rule {
    let kind = resource.map_or(ResourceKind::Terminal, |spawn| spawn.kind);
    let parent = resource.and_then(|spawn| spawn.parent.as_ref());
    if kind_contradicts_binding(kind, parent) {
        return &F_SPAWN_KIND_MISMATCH;
    }
    match parent {
        None => terminal_spawn_rule(satellite.is_some(), owned),
        Some(parent) if is_unowned_agent_session(kind, owned) => {
            agent_session_spawn_rule(satellite, parent)
        }
        // An unknown child kind, or an agent session that also names an
        // owner Terminal: no row covers it.
        Some(_) => &F_UNCLASSIFIED,
    }
}

/// A Terminal has no parent and every other kind has one (L1 §1.2).
const fn kind_contradicts_binding(kind: ResourceKind, parent: Option<&ResourceId>) -> bool {
    kind.is_terminal() == parent.is_some()
}

const fn is_unowned_agent_session(kind: ResourceKind, owned: bool) -> bool {
    matches!(kind, ResourceKind::AgentSession) && !owned
}

fn terminal_spawn_rule(remote: bool, owned: bool) -> &'static Rule {
    match (remote, owned) {
        (false, false) => &F_SPAWN_LOCAL,
        (false, true) => &F_SPAWN_OWNED,
        (true, false) => &F_SPAWN_SATELLITE,
        (true, true) => &F_SPAWN_SATELLITE_OWNED,
    }
}

/// A remote agent session must name a parent on the same satellite; a local
/// or different-host parent is default-deny.
fn agent_session_spawn_rule(
    satellite: Option<&SatelliteHost>,
    parent: &ResourceId,
) -> &'static Rule {
    let Some(host) = satellite else {
        return &F_SPAWN_AGENT_LOCAL;
    };
    if parent_lives_on(parent, host) {
        &F_SPAWN_AGENT_SATELLITE
    } else {
        &F_UNCLASSIFIED
    }
}

fn parent_lives_on(parent: &ResourceId, host: &SatelliteHost) -> bool {
    matches!(parent, ResourceId::Satellite { host: parent_host, .. } if parent_host == host)
}

/// `phux.session.created/v1` and its slash-prefixed one-shot results: the
/// server-owned, connection-private result namespace.
fn is_session_create_result(key: &str) -> bool {
    key == SESSION_CREATE_RESULT_KEY || key.starts_with(SESSION_CREATE_RESULT_KEY_PREFIX)
}

/// Keys the server owns and no client may set or delete, in any scope: the
/// pane occupant, whoami, and the approval records (ADR-0128).
fn is_read_only_server_key(key: &str) -> bool {
    key == RESOURCE_PANE_OCCUPANT_KEY
        || key == crate::wire::frame::RESOURCE_PROGRAM_STATUS_KEY
        || key.starts_with(crate::wire::frame::RESOURCE_PROGRAM_STATUS_RECORD_PREFIX)
        || key == WHOAMI_KEY
        || key.starts_with(APPROVAL_KEY_PREFIX)
}

/// Keys the server applies on write, which are therefore never deletable.
fn is_server_applied_key(key: &str) -> bool {
    key == CONFIG_RELOAD_KEY
        || key == SESSION_KEEP_EMPTY_KEY
        || key.starts_with(APPROVAL_DECIDE_KEY_PREFIX)
}

fn set_metadata_rule(scope: &Scope, key: &str, value: &[u8]) -> &'static Rule {
    if is_session_create_result(key) {
        return &F_RESULT_NAMESPACE_WRITE;
    }
    if is_read_only_server_key(key) {
        return &F_SERVER_OWNED_WRITE;
    }
    if matches!(scope, Scope::Global) {
        global_set_metadata_rule(key, value)
    } else {
        &F_METADATA_WRITE
    }
}

fn global_set_metadata_rule(key: &str, value: &[u8]) -> &'static Rule {
    match key {
        SESSION_CREATE_KEY => &F_SESSION_CREATE,
        SESSION_KEEP_EMPTY_KEY => keep_empty_rule(value),
        CONFIG_RELOAD_KEY => &F_CONFIG_RELOAD,
        _ if key.starts_with(APPROVAL_DECIDE_KEY_PREFIX) => approval_decide_rule(key, value),
        _ => &F_METADATA_WRITE,
    }
}

/// A decision names a canonical approval id and carries `approve` or
/// `deny`; anything else is malformed, refused before a handler parses it.
fn approval_decide_rule(key: &str, value: &[u8]) -> &'static Rule {
    if crate::ids::ApprovalId::from_decide_key(key).is_some() && is_approval_decision(value) {
        &F_APPROVAL_DECIDE
    } else {
        &F_APPROVAL_DECIDE_OTHER
    }
}

/// Setting the mark (`name\0true`) and clearing it (`name\0false`) have rows
/// of their own; any other value is malformed. The value is classified
/// before any handler parses it.
fn keep_empty_rule(value: &[u8]) -> &'static Rule {
    match decode_session_keep_empty(value) {
        Some((_, true)) => &F_KEEP_EMPTY_MARK,
        Some((_, false)) => &F_KEEP_EMPTY_CLEAR,
        None => &F_KEEP_EMPTY_OTHER,
    }
}

/// Reading `phux.whoami/v1` at Global answers only the caller's own identity
/// and grant, so it needs no verb; every other read needs `OBSERVE`.
fn get_metadata_rule(scope: &Scope, key: &str) -> &'static Rule {
    if matches!(scope, Scope::Global) && key == WHOAMI_KEY {
        &F_WHOAMI
    } else {
        &F_GET_METADATA
    }
}

fn delete_metadata_rule(key: &str) -> &'static Rule {
    if is_session_create_result(key) {
        return &F_RESULT_NAMESPACE_WRITE;
    }
    if is_read_only_server_key(key) || is_server_applied_key(key) {
        return &F_SERVER_OWNED_WRITE;
    }
    &F_METADATA_WRITE
}

fn subscribe_metadata_rule(key: &str) -> &'static Rule {
    if is_session_create_result(key) {
        &F_RESULT_NAMESPACE_SUBSCRIBE
    } else {
        &F_SUBSCRIBE_METADATA
    }
}
