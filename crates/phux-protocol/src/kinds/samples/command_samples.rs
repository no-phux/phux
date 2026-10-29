/// A stable index per `Command` variant. Exhaustive: a new variant is a
/// compile error here until it gets an index, then a sample below.
const fn command_variant(command: &Command) -> usize {
    match command {
        Command::AttachResource { .. } => 0,
        Command::DetachResource { .. } => 1,
        Command::KillResource { .. } => 2,
        Command::GetState { .. } => 3,
        Command::GetScreen { .. } => 4,
        Command::RouteInput { .. } => 5,
        Command::ApplyInput { .. } => 6,
        Command::KillResources { .. } => 7,
        Command::CloseTabResources { .. } => 24,
        Command::DetachClients { .. } => 8,
        Command::GetTerminalState { .. } => 9,
        Command::SubscribeResourceEvents { .. } => 10,
        Command::Upgrade => 11,
        Command::Shutdown => 12,
        Command::AcquireInput { .. } => 13,
        Command::ReleaseInput { .. } => 14,
        Command::SignalTerminal { .. } => 15,
        Command::PutFile { .. } => 16,
        Command::ReportAsked { .. } => 17,
        Command::ReportAgentState { .. } => 18,
        Command::GetPerf { .. } => 19,
        Command::Transcribe { .. } => 20,
        Command::AppendResourceOutput { .. } => 21,
        Command::KillResourceIf { .. } => 22,
        Command::OpenListener { .. } => 23,
        Command::GetOrganization { .. } => 25,
        Command::SessionCreate { .. } => 26,
        Command::SessionRename { .. } => 27,
        Command::SessionSetLifetime { .. } => 28,
        Command::SessionDelete { .. } => 29,
        Command::ResourceCreate { .. } => 30,
        Command::ResourceMove { .. } => 31,
        Command::ResourceMoveMany { .. } => 32,
        Command::ResourceRename { .. } => 33,
        Command::ResourceTerminate { .. } => 34,
        Command::ResourceRemove { .. } => 35,
        Command::ResourceRelaunch { .. } => 36,
        Command::InboxResolve { .. } => 37,
        Command::OrganizationUndo { .. } => 38,
        Command::ArrangementGet { .. } => 39,
        Command::ArrangementReplace { .. } => 40,
        Command::DelegateAgent { .. } => 41,
        Command::RevokeDelegation { .. } => 42,
        Command::SessionKill { .. } => 43,
        Command::NativeAgent { .. } => 44,
        Command::PerfControl { .. } => 45,
        Command::SessionSetProject { .. } => 46,
        Command::GetFile { .. } => 47,
        Command::Checkout { .. } => 48,
        Command::ServiceHttpOpen { .. } => 49,
        Command::ServiceHttpRead { .. } => 50,
        Command::ServiceHttpClose { .. } => 51,
        Command::DescribeApi => 52,
        Command::GetCreation { .. } => 53,
    }
}

const COMMAND_VARIANTS: usize = 54;

fn operation() -> IdempotencyKey {
    IdempotencyKey::new([3; 16]).unwrap()
}

fn session_key() -> crate::ids::SessionKey {
    crate::ids::SessionKey::new([4; 16]).unwrap()
}

fn resource_key() -> crate::ids::ResourceKey {
    crate::ids::ResourceKey::new([5; 16]).unwrap()
}

/// One sample per organization command, each on its row.
#[allow(
    clippy::too_many_lines,
    reason = "one sample per organization command, kept as one flat table"
)]
fn org_command_samples() -> Vec<(Command, &'static Rule)> {
    use crate::ids::{ActivationId, ArrangementKey, Revision, ServerInstance};
    use crate::wire::frame::{
        ArrangementDoc, ExitPolicy, LaunchMode, LiveTarget, MoveItem, OrgFilter, SessionLifetime,
        TerminateMode,
    };
    let rev = Revision::new(1);
    let arrangement = ArrangementKey::new([6; 16]).unwrap();
    vec![
        (
            Command::GetOrganization {
                filter: OrgFilter::All,
                page_size: 0,
                cursor: None,
            },
            &C_GET_ORGANIZATION,
        ),
        (
            Command::SessionCreate {
                operation: operation(),
                name: "work".to_owned(),
                lifetime: SessionLifetime::Persistent,
                attrs: Vec::new(),
                launch: None,
            },
            &C_SESSION_CREATE,
        ),
        (
            Command::SessionRename {
                operation: operation(),
                session: session_key(),
                name: "play".to_owned(),
            },
            &C_SESSION_ORGANIZE,
        ),
        (
            Command::SessionSetProject {
                operation: operation(),
                session: session_key(),
                project: Some("phux".to_owned()),
            },
            &C_SESSION_ORGANIZE,
        ),
        (
            Command::SessionSetLifetime {
                operation: operation(),
                session: session_key(),
                lifetime: SessionLifetime::Ephemeral,
            },
            &C_SESSION_ORGANIZE,
        ),
        (
            Command::SessionDelete {
                operation: operation(),
                session: session_key(),
            },
            &C_SESSION_DELETE,
        ),
        (
            Command::SessionKill {
                operation: operation(),
                session: session_key(),
            },
            &C_SESSION_DELETE,
        ),
        (
            Command::ResourceCreate {
                placement: None,
                operation: operation(),
                session: session_key(),
                kind: ResourceKind::Terminal,
                launch: None,
                name: None,
                on_exit: ExitPolicy::Remove,
                launch_mode: LaunchMode::Immediate,
            },
            &C_RESOURCE_CREATE,
        ),
        (Command::ResourceCreate { operation: operation(), session: session_key(), kind: ResourceKind::Terminal, launch: None, name: None, on_exit: ExitPolicy::Retain, launch_mode: LaunchMode::Immediate, placement: Some(Box::new(crate::wire::frame::CreatePlacement::NewGroup)) }, &C_RESOURCE_CREATE_PLACED),
        (
            Command::ResourceMove {
                operation: operation(),
                resource: resource_key(),
                from: session_key(),
                from_expected: rev,
                to: crate::ids::SessionKey::new([9; 16]).unwrap(),
                to_expected: rev,
            },
            &C_RESOURCE_MOVE,
        ),
        (
            Command::ResourceMoveMany {
                operation: operation(),
                items: vec![MoveItem {
                    resource: resource_key(),
                    from: session_key(),
                    from_expected: rev,
                }],
                to: crate::ids::SessionKey::new([9; 16]).unwrap(),
                to_expected: rev,
            },
            &C_RESOURCE_MOVE,
        ),
        (
            Command::ResourceRename {
                operation: operation(),
                resource: resource_key(),
                name: Some("logs".to_owned()),
            },
            &C_RESOURCE_RENAME,
        ),
        (
            Command::ResourceTerminate {
                operation: operation(),
                target: LiveTarget {
                    resource: terminal(),
                    instance: ServerInstance::new([7; 16]),
                    activation: ActivationId::new(1),
                },
                mode: TerminateMode::Graceful,
            },
            &C_RESOURCE_TERMINATE,
        ),
        (
            Command::ResourceRemove {
                operation: operation(),
                resource: resource_key(),
            },
            &C_RESOURCE_REMOVE,
        ),
        (
            Command::ResourceRelaunch {
                operation: operation(),
                resource: resource_key(),
                expected: rev,
            },
            &C_RESOURCE_RELAUNCH,
        ),
        (
            Command::InboxResolve {
                operation: operation(),
                session: session_key(),
                expected: rev,
            },
            &C_SESSION_ORGANIZE,
        ),
        (
            Command::OrganizationUndo {
                operation: operation(),
                undo: IdempotencyKey::new([8; 16]).unwrap(),
                expected: Vec::new(),
            },
            &C_ORGANIZATION_UNDO,
        ),
        (Command::ArrangementGet { arrangement }, &C_ARRANGEMENT_GET),
        (
            Command::ArrangementReplace {
                operation: operation(),
                arrangement,
                expected: rev,
                document: ArrangementDoc::default(),
            },
            &C_ARRANGEMENT_REPLACE,
        ),
        (
            Command::DelegateAgent {
                operation: operation(),
                session: session_key(),
                ttl_secs: 0,
            },
            &C_DELEGATION,
        ),
        (
            Command::RevokeDelegation {
                operation: operation(),
                credential_id: "delegated:00".to_owned(),
            },
            &C_DELEGATION,
        ),
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "one sample per Command variant, kept as one flat table"
)]
fn command_samples() -> Vec<(Command, &'static Rule)> {
    let focus = InputEvent::Focus(FocusEvent::Gained);
    vec![
        (
            Command::NativeAgent {
                request: crate::native_agent::NativeRequestPayload::new(
                    crate::native_agent::NativeRequest::ListIntegrations {},
                )
                .unwrap(),
            },
            &C_NATIVE_LIST,
        ),
        (
            Command::NativeAgent {
                request: crate::native_agent::NativeRequestPayload::new(
                    crate::native_agent::NativeRequest::LaunchIntegration {
                        integration: "opencode".into(),
                        session: session_key(),
                        cwd: "/workspace".into(),
                    },
                )
                .unwrap(),
            },
            &C_NATIVE_LAUNCH,
        ),
        (
            Command::NativeAgent {
                request: crate::native_agent::NativeRequestPayload::new(
                    crate::native_agent::NativeRequest::Spawn {
                        spec: crate::native_agent::SpawnNativeAgent {
                            session: "session".into(),
                            backend: "acp".into(),
                            provider: "omp".into(),
                            command: vec!["omp".into()],
                            cwd: None,
                            env: std::collections::BTreeMap::new(),

                            native_id: None,
                            guardian: None,
                            guardian_key: None,
                            guardian_expected: None,
                        },
                    },
                )
                .unwrap(),
            },
            &C_NATIVE_SPAWN,
        ),
        (
            Command::NativeAgent {
                request: crate::native_agent::NativeRequestPayload::new(
                    crate::native_agent::NativeRequest::Command {
                        agent_session_id: terminal(),
                        command: crate::native_agent::NativeCommand {
                            operation_id: 1,
                            activation: "activation".into(),
                            intent: crate::native_agent::NativeIntent::Prompt {
                                text: "hello".into(),
                            },
                        },
                    },
                )
                .unwrap(),
            },
            &C_NATIVE_COMMAND,
        ),
        (
            Command::NativeAgent {
                request: crate::native_agent::NativeRequestPayload::new(
                    crate::native_agent::NativeRequest::Register {
                        agent_session_id: terminal(),
                        activation: "activation".into(),
                        binding_proof: "proof".into(),
                    },
                )
                .unwrap(),
            },
            &C_NATIVE_CONTROLLER,
        ),
        (
            Command::attach_terminal(terminal(), None),
            &C_ATTACH_RESOURCE,
        ),
        (
            Command::DetachResource {
                terminal_id: terminal(),
            },
            &C_DETACH_RESOURCE,
        ),
        (
            Command::KillResource {
                terminal_id: terminal(),
                operation_id: None,
            },
            &C_KILL_RESOURCE,
        ),
        (
            Command::GetState {
                scope: StateScope::Server,
            },
            &C_GET_STATE,
        ),
        (
            Command::GetScreen {
                terminal_id: terminal(),
                request_scrollback: None,
                cells: false,
                format: 0,
            },
            &C_GET_SCREEN,
        ),
        (
            Command::RouteInput {
                terminal_id: terminal(),
                event: focus.clone(),
            },
            &C_INPUT,
        ),
        (
            Command::ApplyInput {
                operation_id: InputOperationId::new([2; 16]).unwrap(),
                terminal_id: terminal(),
                events: vec![focus],
            },
            &C_INPUT,
        ),
        (
            Command::KillResources {
                ids: vec![terminal()],
                operation_id: None,
            },
            &C_KILL_RESOURCES,
        ),
        (
            Command::DetachClients {
                session: Some("work".to_owned()),
            },
            &C_DETACH_CLIENTS_SESSION,
        ),
        (
            Command::DetachClients { session: None },
            &C_DETACH_CLIENTS_ALL,
        ),
        (
            Command::GetTerminalState {
                terminal_id: terminal(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            },
            &C_GET_TERMINAL_STATE,
        ),
        (
            Command::SubscribeResourceEvents {
                terminal_id: terminal(),
                event_types: Vec::new(),
            },
            &C_SUBSCRIBE_RESOURCE_EVENTS,
        ),
        (Command::Upgrade, &C_UPGRADE),
        (Command::Shutdown, &C_SHUTDOWN),
        (
            Command::AcquireInput {
                terminal_id: terminal(),
                mode: InputMode::Cooperative,
                ttl_ms: 0,
            },
            &C_INPUT_LEASE,
        ),
        (
            Command::ReleaseInput {
                terminal_id: terminal(),
            },
            &C_INPUT_LEASE,
        ),
        (
            Command::SignalTerminal {
                terminal_id: terminal(),
                signal: TerminalSignal::Interrupt,
                operation_id: None,
            },
            &C_SIGNAL_TERMINAL,
        ),
        (
            Command::PutFile {
                upload_id: upload(),
                terminal_id: terminal(),
                extension: "wav".to_owned(),
                offset: 0,
                data: vec![1],
                final_chunk: false,
                sha256: None,
            },
            &C_PUT_FILE,
        ),
        (
            Command::GetFile {
                host: None,
                path: "/tmp/deck.pptx".to_owned(),
                offset: 0,
                max_bytes: 1,
            },
            &C_GET_FILE,
        ),
        (
            Command::Checkout {
                terminal_id: terminal(),
                request: crate::checkout::CheckoutRequest::Stage {
                    paths: vec!["src/main.rs".to_owned()],
                },
            },
            &C_CHECKOUT,
        ),
        (
            Command::ServiceHttpOpen {
                request: Box::new(crate::service_http::ServiceHttpRequest {
                    host: None,
                    service: "opencode".to_owned(),
                    method: "GET".to_owned(),
                    target: "/api/info".to_owned(),
                    headers: Vec::new(),
                    body: Vec::new(),
                }),
            },
            &C_SERVICE_HTTP,
        ),
        (
            Command::ServiceHttpRead {
                exchange: 1,
                max_bytes: 1,
            },
            &C_SERVICE_HTTP,
        ),
        (Command::ServiceHttpClose { exchange: 1 }, &C_SERVICE_HTTP),
        (
            Command::ReportAsked {
                terminal_id: terminal(),
                id: "q".to_owned(),
                question: "continue?".to_owned(),
                suggestions: Vec::new(),
                elapsed_seconds: None,
            },
            &C_AGENT_REPORT,
        ),
        (
            Command::ReportAgentState {
                terminal_id: terminal(),
                state: ReportedAgentState::Working,
            },
            &C_AGENT_REPORT,
        ),
        (Command::DescribeApi, &C_API_DESCRIBE),
        (Command::GetCreation { operation: operation(), session: session_key() }, &C_GET_CREATION),
        (Command::GetPerf { reset: false }, &C_GET_PERF),
        (Command::GetPerf { reset: true }, &C_GET_PERF_RESET),
        (
            Command::PerfControl {
                op: PerfOp::TraceStart,
            },
            &C_PERF_CONTROL,
        ),
        (
            Command::Transcribe {
                upload_id: upload(),
                terminal_id: terminal(),
            },
            &C_TRANSCRIBE,
        ),
        (
            Command::AppendResourceOutput {
                terminal_id: terminal(),
                bytes: b"{}\n".to_vec(),
            },
            &C_APPEND_RESOURCE_OUTPUT,
        ),
        (
            Command::KillResourceIf {
                terminal_id: terminal(),
                precondition: KillPrecondition::default(),
                operation_id: None,
            },
            &C_KILL_RESOURCE_IF,
        ),
        (
            Command::OpenListener {
                transport: ListenerTransport::Quic,
                port_range: None,
                linger_secs: 0,
            },
            &C_OPEN_LISTENER,
        ),
        (
            Command::CloseTabResources {
                ids: vec![terminal()],
            },
            &C_CLOSE_TAB_RESOURCES,
        ),
    ]
    .into_iter()
    .chain(org_command_samples())
    .collect()
}
