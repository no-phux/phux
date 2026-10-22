// -----------------------------------------------------------------------------
// The client-frame table (workload-auth §6, first table), in spec row order.
// -----------------------------------------------------------------------------

const GLOBAL: Subject = Subject::Global {
    owner_uds_only: false,
};
const GLOBAL_OWNER_UDS: Subject = Subject::Global {
    owner_uds_only: true,
};

static F_HELLO: Rule = Rule::exempt("`HELLO`", Exemption::Handshake, Subject::None);
static F_PING: Rule = Rule::exempt("`PING`", Exemption::Liveness, Subject::None);
static F_DETACH: Rule = Rule::exempt("`DETACH`", Exemption::Cleanup, Subject::CallingConnection);
static F_ATTACH: Rule = Rule::verbs(
    "`ATTACH` existing/last target",
    &[Verb::Bind, Verb::Observe],
    Subject::ResolvedGroup,
);
static F_ATTACH_CREATE: Rule = Rule::verbs(
    "`ATTACH` create-if-missing",
    &[Verb::Create, Verb::Bind, Verb::Observe],
    Subject::SelectedLocalGroup,
);
static F_HISTORY_REQUEST: Rule = Rule::verbs(
    "`HISTORY_REQUEST`",
    &[Verb::Observe],
    Subject::NamedTerminal,
);
static F_FRAME_ACK: Rule = Rule::verbs("`FRAME_ACK`", &[Verb::Observe], Subject::NamedTerminal);
static F_COMMAND: Rule = Rule {
    case: "`COMMAND`",
    requirement: Requirement::Nested,
    subject: Subject::None,
};
static F_SUBSCRIBE: Rule = Rule::deny("`SUBSCRIBE` (unallocated)");
static F_INPUT: Rule = Rule::verbs(
    "`INPUT_KEY`, `INPUT_PASTE`, `INPUT_MOUSE`, `INPUT_RAW`, `INPUT_FOCUS`, `INPUT_TERMINAL_REPLY`",
    &[Verb::Input],
    Subject::NamedTerminal,
);
static F_VIEWPORT_RESIZE: Rule = Rule::verbs(
    "`VIEWPORT_RESIZE`",
    &[Verb::Bind],
    Subject::AttachedTerminals,
);
static F_SPAWN_SATELLITE: Rule = Rule::verbs(
    "`SPAWN_RESOURCE { satellite: Some, owner_terminal: None }`",
    &[Verb::Create],
    Subject::SatelliteHost,
);
static F_SPAWN_LOCAL: Rule = Rule::verbs(
    "`SPAWN_RESOURCE { satellite: None, owner_terminal: None }`",
    &[Verb::Create],
    Subject::PayloadGroup,
);
static F_SPAWN_OWNED: Rule = Rule::verbs(
    "`SPAWN_RESOURCE { satellite: None, owner_terminal: Some }`",
    &[Verb::Create, Verb::Bind],
    Subject::OwnerTerminalGroup,
);
static F_SPAWN_SATELLITE_OWNED: Rule =
    Rule::deny("`SPAWN_RESOURCE { satellite: Some, owner_terminal: Some }`");
static F_SPAWN_AGENT_LOCAL: Rule = Rule::verbs(
    "`SPAWN_RESOURCE { kind: AGENT_SESSION, satellite: None, parent: Some }`",
    &[Verb::Create, Verb::Bind],
    Subject::ParentTerminalGroup,
);
static F_SPAWN_AGENT_SATELLITE: Rule = Rule::verbs(
    "`SPAWN_RESOURCE { kind: AGENT_SESSION, satellite: Some(H), parent: Some(Satellite { H, .. }) }`",
    &[Verb::Create, Verb::Bind],
    Subject::SatelliteHostAndParent,
);
static F_SPAWN_KIND_MISMATCH: Rule = Rule::deny(
    "`SPAWN_RESOURCE { kind: not TERMINAL, parent: None }` or `{ kind: TERMINAL, parent: Some }`",
);
static F_RESIZE_TERMINAL: Rule =
    Rule::verbs("`RESIZE_TERMINAL`", &[Verb::Bind], Subject::NamedTerminal);
static F_MOVE_RESOURCE: Rule = Rule::verbs(
    "`MOVE_RESOURCE`",
    &[Verb::Bind],
    Subject::MovedAndOwnerTerminals,
);
static F_SUBSCRIBE_EVENTS_ONE: Rule = Rule::verbs(
    "`SUBSCRIBE_EVENTS { terminal: Some }`",
    &[Verb::Observe],
    Subject::NamedTerminal,
);
static F_SUBSCRIBE_EVENTS_ALL: Rule = Rule::verbs(
    "`SUBSCRIBE_EVENTS { terminal: None }`",
    &[Verb::Observe],
    Subject::ObservableTerminals,
);
static F_WHOAMI: Rule = Rule::exempt(
    r#"`GET_METADATA { Global, "phux.whoami/v1" }`"#,
    Exemption::SelfRead,
    Subject::CallingConnection,
);
static F_GET_METADATA: Rule = Rule::verbs(
    "Other `GET_METADATA`",
    &[Verb::Observe],
    Subject::MetadataScope,
);
static F_SESSION_CREATE: Rule = Rule::verbs(
    r#"`SET_METADATA { Global, "phux.session.create/v1" }`"#,
    &[Verb::Create, Verb::Bind],
    GLOBAL,
);
static F_KEEP_EMPTY_MARK: Rule = Rule::verbs(
    r#"`SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0true`"#,
    &[Verb::Create, Verb::Bind],
    GLOBAL,
);
static F_KEEP_EMPTY_CLEAR: Rule = Rule::verbs(
    r#"`SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0false`"#,
    &[Verb::Signal],
    Subject::NamedSession,
);
static F_KEEP_EMPTY_OTHER: Rule =
    Rule::deny(r#"`SET_METADATA { Global, "phux.session.keep_empty/v1" }` with any other value"#);
static F_CONFIG_RELOAD: Rule = Rule::verbs(
    r#"`SET_METADATA { Global, "phux.config.reload/v1" }`"#,
    &[Verb::Signal],
    GLOBAL,
);
static F_APPROVAL_DECIDE: Rule = Rule::verbs(
    r#"`SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with value `approve` or `deny`"#,
    &[Verb::Signal],
    Subject::HeldAction,
);
static F_APPROVAL_DECIDE_OTHER: Rule = Rule::deny(
    r#"`SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with a malformed id or any other value"#,
);
static F_RESULT_NAMESPACE_WRITE: Rule = Rule::deny(
    "`SET_METADATA` or `DELETE_METADATA` targeting `phux.session.created/v1` or its slash-prefixed results",
);
static F_RESULT_NAMESPACE_SUBSCRIBE: Rule =
    Rule::deny("`SUBSCRIBE_METADATA` targeting that result namespace");
static F_SERVER_OWNED_WRITE: Rule = Rule::deny(
    "`SET_METADATA` or `DELETE_METADATA` targeting `phux.pane-occupant/v1`, `phux.whoami/v1`, or a `phux.approval/v1/<id>` record, or `DELETE_METADATA` targeting `phux.config.reload/v1`, `phux.session.keep_empty/v1`, or a `phux.approval.decide/v1/<id>` key",
);
static F_METADATA_WRITE: Rule = Rule::verbs(
    "Other `SET_METADATA`, `DELETE_METADATA`",
    &[Verb::Bind],
    Subject::MetadataScope,
);
static F_LIST_METADATA: Rule = Rule::verbs(
    "`LIST_METADATA`",
    &[Verb::Inventory],
    Subject::MetadataScope,
);
static F_LIST_DIRECTORY: Rule = Rule::verbs("`LIST_DIRECTORY`", &[Verb::Inventory], GLOBAL);
static F_PATH_QUERY: Rule = Rule::verbs("`PATH_QUERY`", &[Verb::Inventory], GLOBAL);
static F_SUBSCRIBE_METADATA: Rule = Rule::verbs(
    "Other `SUBSCRIBE_METADATA`",
    &[Verb::Observe],
    Subject::MetadataScope,
);
static F_UNCLASSIFIED: Rule =
    Rule::deny("Unknown, wrong-direction, retired, or otherwise unclassified frame");

/// The client-frame table of workload-auth §6, in spec row order.
///
/// [`classify_frame`] returns one of these rows for every decoded frame. It
/// never returns the `COMMAND` row: the nested command decides.
pub static FRAME_RULES: [&Rule; 40] = [
    &F_HELLO,
    &F_PING,
    &F_DETACH,
    &F_ATTACH,
    &F_ATTACH_CREATE,
    &F_HISTORY_REQUEST,
    &F_FRAME_ACK,
    &F_COMMAND,
    &F_SUBSCRIBE,
    &F_INPUT,
    &F_VIEWPORT_RESIZE,
    &F_SPAWN_SATELLITE,
    &F_SPAWN_LOCAL,
    &F_SPAWN_OWNED,
    &F_SPAWN_SATELLITE_OWNED,
    &F_SPAWN_AGENT_LOCAL,
    &F_SPAWN_AGENT_SATELLITE,
    &F_SPAWN_KIND_MISMATCH,
    &F_RESIZE_TERMINAL,
    &F_MOVE_RESOURCE,
    &F_SUBSCRIBE_EVENTS_ONE,
    &F_SUBSCRIBE_EVENTS_ALL,
    &F_WHOAMI,
    &F_GET_METADATA,
    &F_SESSION_CREATE,
    &F_KEEP_EMPTY_MARK,
    &F_KEEP_EMPTY_CLEAR,
    &F_KEEP_EMPTY_OTHER,
    &F_CONFIG_RELOAD,
    &F_APPROVAL_DECIDE,
    &F_APPROVAL_DECIDE_OTHER,
    &F_RESULT_NAMESPACE_WRITE,
    &F_RESULT_NAMESPACE_SUBSCRIBE,
    &F_SERVER_OWNED_WRITE,
    &F_METADATA_WRITE,
    &F_LIST_METADATA,
    &F_LIST_DIRECTORY,
    &F_PATH_QUERY,
    &F_SUBSCRIBE_METADATA,
    &F_UNCLASSIFIED,
];

// -----------------------------------------------------------------------------
// The nested-command table (workload-auth §6, second table), in spec order.
// -----------------------------------------------------------------------------

static C_SPAWN: Rule = Rule::deny("`SPAWN` (unallocated)");
static C_ATTACH_RESOURCE: Rule = Rule::verbs(
    "`ATTACH_RESOURCE`",
    &[Verb::Bind, Verb::Observe],
    Subject::NamedTerminal,
);
static C_DETACH_RESOURCE: Rule = Rule::exempt(
    "`DETACH_RESOURCE`",
    Exemption::Cleanup,
    Subject::CallingConnection,
);
static C_KILL_RESOURCE: Rule =
    Rule::verbs("`KILL_RESOURCE`", &[Verb::Signal], Subject::NamedTerminal);
static C_KILL_RESOURCE_IF: Rule = Rule::verbs(
    "`KILL_RESOURCE_IF`",
    &[Verb::Signal],
    Subject::NamedTerminal,
);
static C_GET_SCREEN: Rule = Rule::verbs("`GET_SCREEN`", &[Verb::Observe], Subject::NamedTerminal);
static C_INPUT: Rule = Rule::verbs(
    "`ROUTE_INPUT`, `APPLY_INPUT`",
    &[Verb::Input],
    Subject::NamedTerminal,
);
static C_KILL_RESOURCES: Rule = Rule::verbs(
    "`KILL_RESOURCES`",
    &[Verb::Signal],
    Subject::EveryNamedTerminal,
);
static C_CLOSE_TAB_RESOURCES: Rule = Rule::verbs(
    "`CLOSE_TAB_RESOURCES`",
    &[Verb::Signal],
    Subject::EveryNamedTerminal,
);
static C_RESIZE_TERMINAL: Rule = Rule::deny("`RESIZE_TERMINAL` (unallocated)");
static C_GET_STATE: Rule = Rule::verbs(
    "`GET_STATE { SERVER }`",
    &[Verb::Inventory],
    Subject::InventoryMatches,
);
static C_RUN_HOOK: Rule = Rule::deny("`RUN_HOOK` (unallocated)");
static C_GET_TERMINAL_STATE: Rule = Rule::verbs(
    "`GET_TERMINAL_STATE`",
    &[Verb::Inventory],
    Subject::NamedTerminal,
);
static C_SUBSCRIBE_RESOURCE_EVENTS: Rule = Rule::verbs(
    "`SUBSCRIBE_RESOURCE_EVENTS`",
    &[Verb::Observe],
    Subject::NamedTerminal,
);
static C_UPGRADE: Rule = Rule::verbs("`UPGRADE`", &[Verb::Signal], GLOBAL);
static C_INPUT_LEASE: Rule = Rule::verbs(
    "`ACQUIRE_INPUT`, `RELEASE_INPUT`",
    &[Verb::Bind],
    Subject::NamedTerminal,
);
static C_SIGNAL_TERMINAL: Rule =
    Rule::verbs("`SIGNAL_TERMINAL`", &[Verb::Signal], Subject::NamedTerminal);
static C_AGENT_REPORT: Rule = Rule::verbs(
    "`REPORT_ASKED`, `REPORT_AGENT_STATE`",
    &[Verb::Bind],
    Subject::NamedTerminal,
);
static C_PUT_FILE: Rule = Rule::verbs("`PUT_FILE`", &[Verb::Input], Subject::NamedTerminal);
static C_GET_FILE: Rule = Rule::verbs("`GET_FILE`", &[Verb::Input], GLOBAL);
// Diffs and file reads are content an `INPUT` holder can already `cat` in the
// named Terminal, and stage/unstage write its checkout's index: `INPUT`, not
// `OBSERVE`, on the Terminal whose working directory selects the checkout.
static C_CHECKOUT: Rule = Rule::verbs("`CHECKOUT`", &[Verb::Input], Subject::NamedTerminal);
// A host service answers as the serving user; an `INPUT` holder on this
// server can already run it from any shell, so `INPUT` on the server.
static C_SERVICE_HTTP: Rule = Rule::verbs(
    "`SERVICE_HTTP_OPEN`, `SERVICE_HTTP_READ`, `SERVICE_HTTP_CLOSE`",
    &[Verb::Input],
    GLOBAL,
);
static C_TRANSCRIBE: Rule = Rule::verbs("`TRANSCRIBE`", &[Verb::Input], Subject::NamedTerminal);
static C_DETACH_CLIENTS_SESSION: Rule = Rule::verbs(
    "`DETACH_CLIENTS { session: Some }`",
    &[Verb::Signal],
    Subject::ResolvedGroup,
);
static C_DETACH_CLIENTS_ALL: Rule = Rule::verbs(
    "`DETACH_CLIENTS { session: None }`",
    &[Verb::Signal],
    GLOBAL,
);
static C_SHUTDOWN: Rule = Rule::verbs("`SHUTDOWN`", &[Verb::Signal], GLOBAL_OWNER_UDS);
static C_OPEN_LISTENER: Rule = Rule::verbs("`OPEN_LISTENER`", &[Verb::Signal], GLOBAL_OWNER_UDS);
static C_GET_PERF: Rule = Rule::verbs("`GET_PERF { reset: false }`", &[Verb::Observe], GLOBAL);
static C_GET_PERF_RESET: Rule = Rule::verbs(
    "`GET_PERF { reset: true }`",
    &[Verb::Observe, Verb::Bind],
    GLOBAL,
);
// Owner socket only: a trace writes files on the server host and heap
// accounting taxes every allocation, which no remote grant should reach.
static C_PERF_CONTROL: Rule = Rule::verbs(
    "`PERF_CONTROL`",
    &[Verb::Observe, Verb::Bind],
    GLOBAL_OWNER_UDS,
);
static C_APPEND_RESOURCE_OUTPUT: Rule = Rule::verbs(
    "`APPEND_RESOURCE_OUTPUT`",
    &[Verb::Bind, Verb::Input],
    Subject::ParentOfNamed,
);
static C_NATIVE_SPAWN: Rule = Rule::verbs(
    "`NATIVE_AGENT { type: spawn }`",
    &[Verb::Create, Verb::Bind],
    Subject::KeyedSession,
);
static C_NATIVE_LIST: Rule = Rule::verbs(
    "`NATIVE_AGENT { type: list_integrations }`",
    &[Verb::Inventory],
    Subject::OrganizationView,
);
static C_NATIVE_LAUNCH: Rule = Rule::verbs(
    "`NATIVE_AGENT { type: launch_integration }`",
    &[Verb::Create, Verb::Bind, Verb::Input],
    Subject::KeyedSession,
);
static C_NATIVE_COMMAND: Rule = Rule::verbs(
    "`NATIVE_AGENT { type: command }`",
    &[Verb::Input],
    Subject::NamedTerminal,
);
static C_NATIVE_CONTROLLER: Rule = Rule::verbs(
    "`NATIVE_AGENT { type: register/receive/complete/publish/close }`",
    &[Verb::Bind],
    Subject::ParentOfNamed,
);
static C_GET_ORGANIZATION: Rule = Rule::verbs(
    "`GET_ORGANIZATION`",
    &[Verb::Inventory],
    Subject::OrganizationView,
);
static C_SESSION_CREATE: Rule = Rule::verbs("`SESSION_CREATE`", &[Verb::Create], GLOBAL);
static C_SESSION_ORGANIZE: Rule = Rule::verbs(
    "`SESSION_RENAME`, `SESSION_SET_PROJECT`, `SESSION_SET_LIFETIME`, `INBOX_RESOLVE`",
    &[Verb::Organize],
    Subject::KeyedSession,
);
static C_SESSION_DELETE: Rule = Rule::verbs(
    "`SESSION_DELETE`, `SESSION_KILL`",
    &[Verb::Terminate],
    Subject::KeyedSession,
);
static C_RESOURCE_CREATE: Rule =
    Rule::verbs("`RESOURCE_CREATE`", &[Verb::Create], Subject::KeyedSession);
static C_RESOURCE_MOVE: Rule = Rule::verbs(
    "`RESOURCE_MOVE`, `RESOURCE_MOVE_MANY`",
    &[Verb::Organize],
    Subject::MoveSessions,
);
static C_RESOURCE_RENAME: Rule = Rule::verbs(
    "`RESOURCE_RENAME`",
    &[Verb::Organize],
    Subject::KeyedResource,
);
static C_RESOURCE_TERMINATE: Rule = Rule::verbs(
    "`RESOURCE_TERMINATE`",
    &[Verb::Terminate],
    Subject::NamedTerminal,
);
static C_RESOURCE_REMOVE: Rule = Rule::verbs(
    "`RESOURCE_REMOVE`",
    &[Verb::Terminate],
    Subject::KeyedResource,
);
static C_RESOURCE_RELAUNCH: Rule = Rule::verbs(
    "`RESOURCE_RELAUNCH`",
    &[Verb::Create],
    Subject::KeyedResource,
);
static C_ORGANIZATION_UNDO: Rule = Rule::verbs(
    "`ORGANIZATION_UNDO`",
    &[Verb::Organize],
    Subject::UndoneOperation,
);
static C_ARRANGEMENT_GET: Rule = Rule::verbs(
    "`ARRANGEMENT_GET`",
    &[Verb::Inventory],
    Subject::ArrangementOwner,
);
static C_ARRANGEMENT_REPLACE: Rule = Rule::verbs(
    "`ARRANGEMENT_REPLACE`",
    &[Verb::Organize],
    Subject::ArrangementOwner,
);
static C_DELEGATION: Rule = Rule::verbs(
    "`DELEGATE_AGENT`, `REVOKE_DELEGATION`",
    &[Verb::Terminate],
    GLOBAL_OWNER_UDS,
);
static C_UNCLASSIFIED: Rule = Rule::deny("Unknown, retired, or otherwise unclassified command tag");

/// The nested-command table of workload-auth §6, in spec row order.
///
/// [`classify_command`] returns one of these rows for every decoded command.
pub static COMMAND_RULES: [&Rule; 51] = [
    &C_SPAWN,
    &C_ATTACH_RESOURCE,
    &C_DETACH_RESOURCE,
    &C_KILL_RESOURCE,
    &C_KILL_RESOURCE_IF,
    &C_GET_SCREEN,
    &C_INPUT,
    &C_KILL_RESOURCES,
    &C_CLOSE_TAB_RESOURCES,
    &C_RESIZE_TERMINAL,
    &C_GET_STATE,
    &C_RUN_HOOK,
    &C_GET_TERMINAL_STATE,
    &C_SUBSCRIBE_RESOURCE_EVENTS,
    &C_UPGRADE,
    &C_INPUT_LEASE,
    &C_SIGNAL_TERMINAL,
    &C_AGENT_REPORT,
    &C_PUT_FILE,
    &C_GET_FILE,
    &C_CHECKOUT,
    &C_SERVICE_HTTP,
    &C_TRANSCRIBE,
    &C_DETACH_CLIENTS_SESSION,
    &C_DETACH_CLIENTS_ALL,
    &C_SHUTDOWN,
    &C_OPEN_LISTENER,
    &C_GET_PERF,
    &C_GET_PERF_RESET,
    &C_PERF_CONTROL,
    &C_APPEND_RESOURCE_OUTPUT,
    &C_NATIVE_SPAWN,
    &C_NATIVE_LIST,
    &C_NATIVE_LAUNCH,
    &C_NATIVE_COMMAND,
    &C_NATIVE_CONTROLLER,
    &C_GET_ORGANIZATION,
    &C_SESSION_CREATE,
    &C_SESSION_ORGANIZE,
    &C_SESSION_DELETE,
    &C_RESOURCE_CREATE,
    &C_RESOURCE_MOVE,
    &C_RESOURCE_RENAME,
    &C_RESOURCE_TERMINATE,
    &C_RESOURCE_REMOVE,
    &C_RESOURCE_RELAUNCH,
    &C_ORGANIZATION_UNDO,
    &C_ARRANGEMENT_GET,
    &C_ARRANGEMENT_REPLACE,
    &C_DELEGATION,
    &C_UNCLASSIFIED,
];
