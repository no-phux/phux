/// Declares each row as a `static` and `$table` as every row, in order.
/// Verbs and subjects name [`Verb`] and [`Subject`] variants; `Global` and
/// `GlobalOwnerUds` are the Global selector without and with the owner-UDS
/// transport predicate.
macro_rules! rule_table {
    ($(#[$meta:meta])* $table:ident; $($name:ident = $kind:ident($($arg:tt)*);)*) => {
        $(static $name: Rule = rule_table!(@$kind $($arg)*);)*
        $(#[$meta])*
        pub static $table: [&Rule; [$(stringify!($name)),*].len()] = [$(&$name),*];
    };
    (@verbs $case:expr, [$($verb:ident),+], $subject:ident $(,)?) => {
        Rule::verbs($case, &[$(Verb::$verb),+], rule_table!(@subject $subject))
    };
    (@exempt $case:expr, $exemption:ident, $subject:ident $(,)?) => {
        Rule::exempt($case, Exemption::$exemption, Subject::$subject)
    };
    (@deny $case:expr $(,)?) => {
        Rule::deny($case)
    };
    (@nested $case:expr) => {
        Rule {
            case: $case,
            requirement: Requirement::Nested,
            subject: Subject::None,
        }
    };
    (@subject Global) => {
        Subject::Global { owner_uds_only: false }
    };
    (@subject GlobalOwnerUds) => {
        Subject::Global { owner_uds_only: true }
    };
    (@subject $subject:ident) => {
        Subject::$subject
    };
}

rule_table! {
    /// The client-frame table, in spec row order.
    ///
    /// [`classify_frame`] returns one of these rows for every decoded frame. It
    /// never returns the `COMMAND` row: the nested command decides.
    FRAME_RULES;
    F_HELLO = exempt("`HELLO`", Handshake, None);
    F_PING = exempt("`PING`", Liveness, None);
    F_DETACH = exempt("`DETACH`", Cleanup, CallingConnection);
    F_ATTACH = verbs("`ATTACH` existing/last target", [Bind, Observe], ResolvedGroup);
    F_ATTACH_CREATE = verbs(
        "`ATTACH` create-if-missing", [Create, Bind, Observe], SelectedLocalGroup
    );
    F_HISTORY_REQUEST = verbs("`HISTORY_REQUEST`", [Observe], NamedTerminal);
    F_FRAME_ACK = verbs("`FRAME_ACK`", [Observe], NamedTerminal);
    F_COMMAND = nested("`COMMAND`");
    F_SUBSCRIBE = deny("`SUBSCRIBE` (unallocated)");
    F_INPUT = verbs(
        "`INPUT_KEY`, `INPUT_PASTE`, `INPUT_MOUSE`, `INPUT_RAW`, `INPUT_FOCUS`, `INPUT_TERMINAL_REPLY`, `CLIPBOARD_REPLY`",
        [Input],
        NamedTerminal,
    );
    F_VIEWPORT_RESIZE = verbs("`VIEWPORT_RESIZE`", [Bind], AttachedTerminals);
    F_VIEWPORT_COLORS = verbs("`VIEWPORT_COLORS`", [Bind], AttachedTerminals);
    F_SPAWN_SATELLITE = verbs(
        "`SPAWN_RESOURCE { satellite: Some, owner_terminal: None }`", [Create], SatelliteHost
    );
    F_SPAWN_LOCAL = verbs(
        "`SPAWN_RESOURCE { satellite: None, owner_terminal: None }`", [Create], PayloadGroup
    );
    F_SPAWN_OWNED = verbs(
        "`SPAWN_RESOURCE { satellite: None, owner_terminal: Some }`",
        [Create, Bind],
        OwnerTerminalGroup,
    );
    F_SPAWN_SATELLITE_OWNED = deny("`SPAWN_RESOURCE { satellite: Some, owner_terminal: Some }`");
    F_SPAWN_CHILD_LOCAL = verbs(
        "`SPAWN_RESOURCE { kind: served non-TERMINAL, satellite: None, parent: Some }`",
        [Create, Bind],
        ParentTerminalGroup,
    );
    F_SPAWN_CHILD_SATELLITE = verbs(
        "`SPAWN_RESOURCE { kind: served non-TERMINAL, satellite: Some(H), parent: Some(Satellite { H, .. }) }`",
        [Create, Bind],
        SatelliteHostAndParent,
    );
    F_SPAWN_KIND_MISMATCH = deny(
        "`SPAWN_RESOURCE { kind: not TERMINAL, parent: None }` or `{ kind: TERMINAL, parent: Some }`",
    );
    // Declaring a control class (page, window, display) claims what viewers'
    // `input@CLASS` grants then reach: only the owner socket opens one
    // (`docs/authority.md`).
    F_SPAWN_MACHINE_CONTROL = deny("`SPAWN_RESOURCE { control: Some }`");
    F_RESIZE_TERMINAL = verbs("`RESIZE_TERMINAL`", [Bind], NamedTerminal);
    F_MOVE_RESOURCE = verbs("`MOVE_RESOURCE`", [Bind], MovedAndOwnerTerminals);
    F_SUBSCRIBE_EVENTS_ONE = verbs(
        "`SUBSCRIBE_EVENTS { terminal: Some }`", [Observe], NamedTerminal
    );
    F_SUBSCRIBE_EVENTS_ALL = verbs(
        "`SUBSCRIBE_EVENTS { terminal: None }`", [Observe], ObservableTerminals
    );
    F_WHOAMI = exempt(
        r#"`GET_METADATA { Global, "phux.whoami/v1" }`"#, SelfRead, CallingConnection
    );
    F_GET_METADATA = verbs("Other `GET_METADATA`", [Observe], MetadataScope);
    F_SESSION_CREATE = verbs(
        r#"`SET_METADATA { Global, "phux.session.create/v1" }`"#, [Create, Bind], Global
    );
    F_KEEP_EMPTY_MARK = verbs(
        r#"`SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0true`"#,
        [Create, Bind],
        Global,
    );
    F_KEEP_EMPTY_CLEAR = verbs(
        r#"`SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0false`"#,
        [Signal],
        NamedSession,
    );
    F_KEEP_EMPTY_OTHER = deny(
        r#"`SET_METADATA { Global, "phux.session.keep_empty/v1" }` with any other value"#
    );
    F_CONFIG_RELOAD = verbs(
        r#"`SET_METADATA { Global, "phux.config.reload/v1" }`"#, [Signal], Global
    );
    F_APPROVAL_DECIDE = verbs(
        r#"`SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with value `approve` or `deny`"#,
        [Signal],
        HeldAction,
    );
    F_APPROVAL_DECIDE_OTHER = deny(
        r#"`SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with a malformed id or any other value"#,
    );
    F_RESULT_NAMESPACE_WRITE = deny(
        "`SET_METADATA` or `DELETE_METADATA` targeting `phux.session.created/v1` or its slash-prefixed results",
    );
    F_RESULT_NAMESPACE_SUBSCRIBE = deny("`SUBSCRIBE_METADATA` targeting that result namespace");
    F_SERVER_OWNED_WRITE = deny(
        "`SET_METADATA` or `DELETE_METADATA` targeting `phux.pane-occupant/v1`, `phux.controller/v1`, `phux.whoami/v1`, or a `phux.approval/v1/<id>` record, or `DELETE_METADATA` targeting `phux.config.reload/v1`, `phux.session.keep_empty/v1`, or a `phux.approval.decide/v1/<id>` key",
    );
    F_METADATA_WRITE = verbs("Other `SET_METADATA`, `DELETE_METADATA`", [Bind], MetadataScope);
    F_LIST_METADATA = verbs("`LIST_METADATA`", [Inventory], MetadataScope);
    F_LIST_DIRECTORY = verbs("`LIST_DIRECTORY`", [Inventory], Global);
    F_PATH_QUERY = verbs("`PATH_QUERY`", [Inventory], Global);
    F_SUBSCRIBE_METADATA = verbs("Other `SUBSCRIBE_METADATA`", [Observe], MetadataScope);
    F_UNCLASSIFIED = deny("Unknown, wrong-direction, retired, or otherwise unclassified frame");
}

rule_table! {
    /// The nested-command table, in spec row order.
    ///
    /// [`classify_command`] returns one of these rows for every decoded command.
    COMMAND_RULES;
    C_SPAWN = deny("`SPAWN` (unallocated)");
    // `OBSERVE` on a named window or display document needs a grant naming its
    // class, never its parent; the server splits that verb off this row and the
    // other observe rows (`docs/authority.md#watching`).
    C_ATTACH_RESOURCE = verbs("`ATTACH_RESOURCE`", [Bind, Observe], NamedTerminal);
    C_DETACH_RESOURCE = exempt("`DETACH_RESOURCE`", Cleanup, CallingConnection);
    C_KILL_RESOURCE = verbs("`KILL_RESOURCE`", [Signal], NamedTerminal);
    C_KILL_RESOURCE_IF = verbs("`KILL_RESOURCE_IF`", [Signal], NamedTerminal);
    C_GET_SCREEN = verbs("`GET_SCREEN`", [Observe], NamedTerminal);
    C_INPUT = verbs("`ROUTE_INPUT`, `APPLY_INPUT`", [Input], NamedTerminal);
    C_KILL_RESOURCES = verbs("`KILL_RESOURCES`", [Signal], EveryNamedTerminal);
    C_CLOSE_TAB_RESOURCES = verbs("`CLOSE_TAB_RESOURCES`", [Signal], EveryNamedTerminal);
    C_RESIZE_TERMINAL = deny("`RESIZE_TERMINAL` (unallocated)");
    C_GET_STATE = verbs("`GET_STATE { SERVER }`", [Inventory], InventoryMatches);
    C_RUN_HOOK = deny("`RUN_HOOK` (unallocated)");
    C_GET_TERMINAL_STATE = verbs("`GET_TERMINAL_STATE`", [Inventory], NamedTerminal);
    C_SUBSCRIBE_RESOURCE_EVENTS = verbs("`SUBSCRIBE_RESOURCE_EVENTS`", [Observe], NamedTerminal);
    C_UPGRADE = verbs("`UPGRADE`", [Signal], Global);
    C_INPUT_LEASE = verbs("`ACQUIRE_INPUT`, `RELEASE_INPUT`", [Bind], NamedTerminal);
    C_SIGNAL_TERMINAL = verbs("`SIGNAL_TERMINAL`", [Signal], NamedTerminal);
    C_AGENT_REPORT = verbs("`REPORT_ASKED`, `REPORT_AGENT_STATE`", [Bind], NamedTerminal);
    C_PUT_FILE = verbs("`PUT_FILE`", [Input], NamedTerminal);
    C_GET_FILE = verbs("`GET_FILE`", [Input], Global);
    // Diffs and file reads are content an `INPUT` holder can already `cat` in the
    // named Terminal, and stage/unstage write its checkout's index: `INPUT`, not
    // `OBSERVE`, on the Terminal whose working directory selects the checkout.
    C_CHECKOUT = verbs("`CHECKOUT`", [Input], NamedTerminal);
    // A host service answers as the serving user; an `INPUT` holder on this
    // server can already run it from any shell, so `INPUT` on the server.
    C_SERVICE_HTTP = verbs(
        "`SERVICE_HTTP_OPEN`, `SERVICE_HTTP_READ`, `SERVICE_HTTP_CLOSE`", [Input], Global
    );
    // The server dials as the serving user, which an `INPUT` holder on this
    // server can already do from any shell (`nc`): `INPUT` on the server.
    C_FORWARD = verbs(
        "`FORWARD_OPEN`, `FORWARD_READ`, `FORWARD_WRITE`, `FORWARD_CLOSE`, `AGENT_OFFER`, `AGENT_REPLY`",
        [Input],
        Global,
    );
    C_TRANSCRIBE = verbs("`TRANSCRIBE`", [Input], NamedTerminal);
    C_DETACH_CLIENTS_SESSION = verbs("`DETACH_CLIENTS { session: Some }`", [Signal], ResolvedGroup);
    C_DETACH_CLIENTS_ALL = verbs("`DETACH_CLIENTS { session: None }`", [Signal], Global);
    C_SHUTDOWN = verbs("`SHUTDOWN`", [Signal], GlobalOwnerUds);
    C_OPEN_LISTENER = verbs("`OPEN_LISTENER`", [Signal], GlobalOwnerUds);
    C_GET_PERF = verbs("`GET_PERF { reset: false }`", [Observe], Global);
    C_GET_PERF_RESET = verbs("`GET_PERF { reset: true }`", [Observe, Bind], Global);
    // Owner socket only: a trace writes files on the server host and heap
    // accounting taxes every allocation, which no remote grant should reach.
    C_PERF_CONTROL = verbs("`PERF_CONTROL`", [Observe, Bind], GlobalOwnerUds);
    C_APPEND_RESOURCE_OUTPUT = verbs("`APPEND_RESOURCE_OUTPUT`", [Bind, Input], ParentOfNamed);
    C_PUBLISH_RESOURCE_STATE = verbs("`PUBLISH_RESOURCE_STATE`", [Bind, Input], ParentOfNamed);
    // Input to a page, window or display is control of what it shows, so no
    // terminal grant reaches it: only a grant naming the document's control
    // class, or the document itself (`docs/resource-kinds.md#authority`).
    C_SEND_DOCUMENT_INPUT = verbs("`SEND_DOCUMENT_INPUT`", [Input], ControlledDocument);
    // Listing, starting or stopping a host's mirrors shows its screen: a
    // control selector of the class the target falls in (`window`, or
    // `display` for `display[:OUTPUT]`; listing is `window`), as for the
    // documents it opens. Rune's macOS runner reports from the owner socket.
    C_MIRROR = verbs("`MIRROR`", [Observe], ControlledDocument);
    C_MIRROR_PROVIDE = verbs("`MIRROR_PROVIDE`", [Observe], GlobalOwnerUds);
    // Which grants the host's server holds says what its panes can read: owner,
    // Unix socket only.
    C_PRIVACY_STATUS = verbs("`PRIVACY_STATUS`", [Inventory], GlobalOwnerUds);
    // Full Global control, never pane-scoped agent keys or control-class grants.
    // Every Request also goes through the mandatory per-exchange approval gate.
    C_CREDENTIALS = verbs("`CREDENTIALS`", [Bind, Input], Global);
    // What works on the host, for a person deciding what to fix: inventory, so a
    // paired phone reads it like `GET_STATE`.
    C_HOST_CHECKLIST = verbs("`HOST_CHECKLIST`", [Inventory], Global);
    C_NATIVE_SPAWN = verbs("`NATIVE_AGENT { type: spawn }`", [Create, Bind], KeyedSession);
    C_NATIVE_LIST = verbs(
        "`NATIVE_AGENT { type: list_integrations }`", [Inventory], OrganizationView
    );
    C_NATIVE_LAUNCH = verbs(
        "`NATIVE_AGENT { type: launch_integration }`", [Create, Bind, Input], KeyedSession
    );
    C_NATIVE_COMMAND = verbs("`NATIVE_AGENT { type: command }`", [Input], NamedTerminal);
    C_CURRENT_QUESTIONS = verbs(
        "`NATIVE_AGENT { type: current_questions }` with handle", [Observe], NamedTerminal
    );
    C_CURRENT_QUESTIONS_KEYED = verbs(
        "`NATIVE_AGENT { type: current_questions }` with durable key", [Observe], KeyedResource
    );
    C_RESPOND_QUESTION = verbs(
        "`NATIVE_AGENT { type: respond_question }`", [Observe, Input], KeyedResource
    );
    C_QUESTION_RECEIPT = verbs(
        "`NATIVE_AGENT { type: question_receipt }`", [Observe], KeyedResource
    );
    C_NATIVE_CONTROLLER = verbs(
        "`NATIVE_AGENT { type: register/receive/complete/publish/close }`", [Bind], ParentOfNamed
    );
    C_GET_ORGANIZATION = verbs("`GET_ORGANIZATION`", [Inventory], OrganizationView);
    C_API_DESCRIBE = verbs("`DESCRIBE_API`", [Inventory], Global);
    C_GET_CREATION = verbs("`GET_CREATION`", [Inventory], KeyedSession);
    C_SESSION_CREATE = verbs("`SESSION_CREATE`", [Create], Global);
    C_SESSION_ORGANIZE = verbs(
        "`SESSION_RENAME`, `SESSION_SET_PROJECT`, `SESSION_SET_LIFETIME`, `INBOX_RESOLVE`",
        [Organize],
        KeyedSession,
    );
    C_SESSION_DELETE = verbs("`SESSION_DELETE`, `SESSION_KILL`", [Terminate], KeyedSession);
    C_RESOURCE_CREATE = verbs("`RESOURCE_CREATE`", [Create], KeyedSession);
    C_RESOURCE_CREATE_PLACED = verbs(
        "`RESOURCE_CREATE` with placement", [Create, Organize], KeyedSession
    );
    C_RESOURCE_MOVE = verbs("`RESOURCE_MOVE`, `RESOURCE_MOVE_MANY`", [Organize], MoveSessions);
    C_RESOURCE_RENAME = verbs("`RESOURCE_RENAME`", [Organize], KeyedResource);
    C_RESOURCE_TERMINATE = verbs("`RESOURCE_TERMINATE`", [Terminate], NamedTerminal);
    C_RESOURCE_REMOVE = verbs("`RESOURCE_REMOVE`", [Terminate], KeyedResource);
    C_RESOURCE_RELAUNCH = verbs("`RESOURCE_RELAUNCH`", [Create], KeyedResource);
    C_ORGANIZATION_UNDO = verbs("`ORGANIZATION_UNDO`", [Organize], UndoneOperation);
    C_ARRANGEMENT_GET = verbs("`ARRANGEMENT_GET`", [Inventory], ArrangementOwner);
    C_ARRANGEMENT_REPLACE = verbs(
        "`ARRANGEMENT_REPLACE`, `ARRANGEMENT_LOCK`", [Organize], ArrangementOwner
    );
    C_DELEGATION = verbs("`DELEGATE_AGENT`, `REVOKE_DELEGATION`", [Terminate], GlobalOwnerUds);
    C_UNCLASSIFIED = deny("Unknown, retired, or otherwise unclassified command tag");
}
