/// The gate of every organization method: `features_ext.ORGANIZATION`.
const ORGANIZATION: Option<FeatureGate> = Some(FeatureGate::FeaturesExt {
    mask: ServerFeatureExt::Organization.mask(),
    wire_name: ServerFeatureExt::Organization.wire_name(),
});

/// The substrate every kind shares (L1 §1.1): spawn and close, the output
/// subscription, kill, events, inventory, and the resource's L3 scope.
pub static SUBSTRATE_METHODS: &[MethodSpec] = &[
    method!(
        "SPAWN_RESOURCE",
        Carrier::Frame(TYPE_SPAWN_RESOURCE),
        [
            F_SPAWN_SATELLITE,
            F_SPAWN_LOCAL,
            F_SPAWN_OWNED,
            F_SPAWN_SATELLITE_OWNED,
            F_SPAWN_AGENT_LOCAL,
            F_SPAWN_AGENT_SATELLITE,
            F_SPAWN_KIND_MISMATCH,
            F_UNCLASSIFIED,
        ]
    ),
    method!(
        "ATTACH_RESOURCE",
        Carrier::Command(COMMAND_TAG_ATTACH_RESOURCE),
        [C_ATTACH_RESOURCE]
    ),
    method!(
        "DETACH_RESOURCE",
        Carrier::Command(COMMAND_TAG_DETACH_RESOURCE),
        [C_DETACH_RESOURCE]
    ),
    dangerous(method!(
        "KILL_RESOURCE",
        Carrier::Command(COMMAND_TAG_KILL_RESOURCE),
        [C_KILL_RESOURCE]
    )),
    dangerous(method!(
        "KILL_RESOURCE_IF",
        Carrier::Command(COMMAND_TAG_KILL_RESOURCE_IF),
        [C_KILL_RESOURCE_IF],
        Some(FeatureGate::Features(ServerFeature::ConditionalKill))
    )),
    dangerous(method!(
        "KILL_RESOURCES",
        Carrier::Command(COMMAND_TAG_KILL_RESOURCES),
        [C_KILL_RESOURCES]
    )),
    dangerous(method!(
        "CLOSE_TAB_RESOURCES",
        Carrier::Command(COMMAND_TAG_CLOSE_TAB_RESOURCES),
        [C_CLOSE_TAB_RESOURCES],
        Some(FeatureGate::Features(ServerFeature::CloseTabResources))
    )),
    method!(
        "SUBSCRIBE_RESOURCE_EVENTS",
        Carrier::Command(COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS),
        [C_SUBSCRIBE_RESOURCE_EVENTS]
    ),
    method!(
        "SUBSCRIBE_EVENTS",
        Carrier::Frame(TYPE_SUBSCRIBE_EVENTS),
        [F_SUBSCRIBE_EVENTS_ONE, F_SUBSCRIBE_EVENTS_ALL]
    ),
    method!(
        "GET_STATE",
        Carrier::Command(COMMAND_TAG_GET_STATE),
        [C_GET_STATE]
    ),
    method!(
        "GET_METADATA",
        Carrier::Frame(TYPE_GET_METADATA),
        [F_GET_METADATA, F_WHOAMI]
    ),
    method!(
        "SET_METADATA",
        Carrier::Frame(TYPE_SET_METADATA),
        [
            F_METADATA_WRITE,
            F_SESSION_CREATE,
            F_KEEP_EMPTY_MARK,
            F_KEEP_EMPTY_CLEAR,
            F_KEEP_EMPTY_OTHER,
            F_CONFIG_RELOAD,
            F_APPROVAL_DECIDE,
            F_APPROVAL_DECIDE_OTHER,
            F_RESULT_NAMESPACE_WRITE,
            F_SERVER_OWNED_WRITE,
        ]
    ),
    method!(
        "DELETE_METADATA",
        Carrier::Frame(TYPE_DELETE_METADATA),
        [
            F_METADATA_WRITE,
            F_RESULT_NAMESPACE_WRITE,
            F_SERVER_OWNED_WRITE
        ]
    ),
    method!(
        "LIST_METADATA",
        Carrier::Frame(TYPE_LIST_METADATA),
        [F_LIST_METADATA]
    ),
    method!(
        "SUBSCRIBE_METADATA",
        Carrier::Frame(TYPE_SUBSCRIBE_METADATA),
        [F_SUBSCRIBE_METADATA, F_RESULT_NAMESPACE_SUBSCRIBE]
    ),
];

/// Events about the server or one subscription rather than one resource.
///
/// `journal_gap` tells a subscriber which journal `seq` range it missed; it
/// is never journaled itself (L1 §7.3). `approval_requested` and
/// `approval_decided` report a held `SIGNAL` action (ADR-0128). The
/// organization events (L1 §5.7) report committed changes to keyed records,
/// which outlive any one live resource.
pub static SERVER_EVENTS: &[EventSpec] = &[
    EventSpec {
        name: "journal_gap",
        tag: EVENT_TAG_JOURNAL_GAP,
    },
    EventSpec {
        name: "approval_requested",
        tag: EVENT_TAG_APPROVAL_REQUESTED,
    },
    EventSpec {
        name: "approval_decided",
        tag: EVENT_TAG_APPROVAL_DECIDED,
    },
    EventSpec {
        name: "session_created",
        tag: EVENT_TAG_SESSION_CREATED,
    },
    EventSpec {
        name: "session_renamed",
        tag: EVENT_TAG_SESSION_RENAMED,
    },
    EventSpec {
        name: "session_deleted",
        tag: EVENT_TAG_SESSION_DELETED,
    },
    EventSpec {
        name: "session_lifetime_changed",
        tag: EVENT_TAG_SESSION_LIFETIME_CHANGED,
    },
    EventSpec {
        name: "membership_changed",
        tag: EVENT_TAG_MEMBERSHIP_CHANGED,
    },
    EventSpec {
        name: "resource_created",
        tag: EVENT_TAG_RESOURCE_CREATED,
    },
    EventSpec {
        name: "resource_removed",
        tag: EVENT_TAG_RESOURCE_REMOVED,
    },
    EventSpec {
        name: "activation_changed",
        tag: EVENT_TAG_ACTIVATION_CHANGED,
    },
    EventSpec {
        name: "arrangement_changed",
        tag: EVENT_TAG_ARRANGEMENT_CHANGED,
    },
    EventSpec {
        name: "lease_changed",
        tag: EVENT_TAG_LEASE_CHANGED,
    },
    EventSpec {
        name: "intake_changed",
        tag: EVENT_TAG_INTAKE_CHANGED,
    },
    EventSpec {
        name: "resource_renamed",
        tag: EVENT_TAG_RESOURCE_RENAMED,
    },
    EventSpec {
        name: "session_project_set",
        tag: EVENT_TAG_SESSION_PROJECT_SET,
    },
];

/// The events every kind shares: its own spawn and close, and `source_gap`
/// when the resource produced events faster than the server could journal
/// them (L1 §7.3).
pub static SUBSTRATE_EVENTS: &[EventSpec] = &[
    EventSpec {
        name: "pane_spawned",
        tag: EVENT_TAG_RESOURCE_SPAWNED,
    },
    EventSpec {
        name: "pane_closed",
        tag: EVENT_TAG_RESOURCE_CLOSED,
    },
    EventSpec {
        name: "source_gap",
        tag: EVENT_TAG_SOURCE_GAP,
    },
];

/// Every resource kind this build serves, in wire-tag order.
pub static KINDS: [KindSpec; 2] = [
    KindSpec {
        kind: ResourceKind::Terminal,
        name: "TERMINAL",
        gate: None,
        methods: &[
            method!("INPUT_KEY", Carrier::Frame(TYPE_INPUT_KEY), [F_INPUT]),
            method!("INPUT_PASTE", Carrier::Frame(TYPE_INPUT_PASTE), [F_INPUT]),
            method!("INPUT_MOUSE", Carrier::Frame(TYPE_INPUT_MOUSE), [F_INPUT]),
            method!("INPUT_FOCUS", Carrier::Frame(TYPE_INPUT_FOCUS), [F_INPUT]),
            MethodSpec {
                name: "INPUT_RAW",
                carrier: Carrier::Frame(INPUT_RAW_RESERVED_TYPE),
                rules: &[&F_INPUT],
                gate: None,
                shipped: false,
                dangerous: false,
            },
            method!(
                "INPUT_TERMINAL_REPLY",
                Carrier::Frame(TYPE_INPUT_TERMINAL_REPLY),
                [F_INPUT],
                Some(FeatureGate::Features(ServerFeature::TerminalReply))
            ),
            method!(
                "RESIZE_TERMINAL",
                Carrier::Frame(TYPE_RESIZE_TERMINAL),
                [F_RESIZE_TERMINAL]
            ),
            method!(
                "HISTORY_REQUEST",
                Carrier::Frame(TYPE_HISTORY_REQUEST),
                [F_HISTORY_REQUEST]
            ),
            method!("FRAME_ACK", Carrier::Frame(TYPE_FRAME_ACK), [F_FRAME_ACK]),
            method!(
                "MOVE_RESOURCE",
                Carrier::Frame(TYPE_MOVE_RESOURCE),
                [F_MOVE_RESOURCE],
                Some(FeatureGate::Features(ServerFeature::MoveResource))
            ),
            method!(
                "GET_SCREEN",
                Carrier::Command(COMMAND_TAG_GET_SCREEN),
                [C_GET_SCREEN]
            ),
            method!(
                "ROUTE_INPUT",
                Carrier::Command(COMMAND_TAG_ROUTE_INPUT),
                [C_INPUT]
            ),
            method!(
                "APPLY_INPUT",
                Carrier::Command(COMMAND_TAG_APPLY_INPUT),
                [C_INPUT],
                Some(FeatureGate::Features(ServerFeature::AcknowledgedInput))
            ),
            method!(
                "PUT_FILE",
                Carrier::Command(COMMAND_TAG_PUT_FILE),
                [C_PUT_FILE],
                Some(FeatureGate::Features(ServerFeature::FileUpload))
            ),
            method!(
                "CHECKOUT",
                Carrier::Command(COMMAND_TAG_CHECKOUT),
                [C_CHECKOUT],
                Some(FeatureGate::FeaturesExt {
                    mask: ServerFeatureExt::Checkout.mask(),
                    wire_name: ServerFeatureExt::Checkout.wire_name(),
                })
            ),
            method!(
                "TRANSCRIBE",
                Carrier::Command(COMMAND_TAG_TRANSCRIBE),
                [C_TRANSCRIBE],
                Some(FeatureGate::Features(ServerFeature::Transcribe))
            ),
            method!(
                "GET_TERMINAL_STATE",
                Carrier::Command(COMMAND_TAG_GET_TERMINAL_STATE),
                [C_GET_TERMINAL_STATE]
            ),
            method!(
                "ACQUIRE_INPUT",
                Carrier::Command(COMMAND_TAG_ACQUIRE_INPUT),
                [C_INPUT_LEASE]
            ),
            method!(
                "RELEASE_INPUT",
                Carrier::Command(COMMAND_TAG_RELEASE_INPUT),
                [C_INPUT_LEASE]
            ),
            dangerous(method!(
                "SIGNAL_TERMINAL",
                Carrier::Command(COMMAND_TAG_SIGNAL_TERMINAL),
                [C_SIGNAL_TERMINAL]
            )),
            method!(
                "REPORT_ASKED",
                Carrier::Command(COMMAND_TAG_REPORT_ASKED),
                [C_AGENT_REPORT]
            ),
            method!(
                "REPORT_AGENT_STATE",
                Carrier::Command(COMMAND_TAG_REPORT_AGENT_STATE),
                [C_AGENT_REPORT],
                Some(FeatureGate::Features(ServerFeature::ReportAgentState))
            ),
        ],
        events: &[
            EventSpec {
                name: "command_started",
                tag: EVENT_TAG_COMMAND_STARTED,
            },
            EventSpec {
                name: "command_finished",
                tag: EVENT_TAG_COMMAND_FINISHED,
            },
            EventSpec {
                name: "title_changed",
                tag: EVENT_TAG_TITLE_CHANGED,
            },
            EventSpec {
                name: "bell",
                tag: EVENT_TAG_BELL,
            },
            EventSpec {
                name: "dirty",
                tag: EVENT_TAG_DIRTY,
            },
            EventSpec {
                name: "idle",
                tag: EVENT_TAG_IDLE,
            },
            EventSpec {
                name: "terminal_control",
                tag: EVENT_TAG_TERMINAL_CONTROL,
            },
            EventSpec {
                name: "asked",
                tag: EVENT_TAG_ASKED,
            },
            EventSpec {
                name: "cwd_changed",
                tag: EVENT_TAG_CWD_CHANGED,
            },
            EventSpec {
                name: "input_activity",
                tag: EVENT_TAG_INPUT_ACTIVITY,
            },
        ],
        metadata_keys: &[
            RESOURCE_AGENT_KEY,
            RESOURCE_AGENT_SESSION_KEY,
            RESOURCE_PANE_OCCUPANT_KEY,
            RESOURCE_TAGS_KEY,
            RESOURCE_LINK_KEY,
        ],
    },
    KindSpec {
        kind: ResourceKind::AgentSession,
        name: "AGENT_SESSION",
        gate: Some(FeatureGate::Features(ServerFeature::ResourceKinds)),
        methods: &[
            method!(
                "APPEND_RESOURCE_OUTPUT",
                Carrier::Command(COMMAND_TAG_APPEND_RESOURCE_OUTPUT),
                [C_APPEND_RESOURCE_OUTPUT],
                Some(FeatureGate::Features(ServerFeature::ResourceKinds))
            ),
            method!(
                "NATIVE_AGENT",
                Carrier::Command(crate::wire::frame::COMMAND_TAG_NATIVE_AGENT),
                [
                    C_NATIVE_SPAWN,
                    C_NATIVE_LIST,
                    C_NATIVE_LAUNCH,
                    C_NATIVE_COMMAND,
                    C_NATIVE_CONTROLLER
                ],
                None
            ),
        ],
        events: &[EventSpec {
            name: "native_agent_result",
            tag: crate::wire::frame::EVENT_TAG_NATIVE_AGENT_RESULT,
        }],
        metadata_keys: &[],
    },
];
