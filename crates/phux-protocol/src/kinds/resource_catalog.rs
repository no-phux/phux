const ACCESSIBILITY_TREE: FeatureGate = FeatureGate::ext(ServerFeatureExt::AccessibilityTree);
const PIXEL_SURFACE: FeatureGate = FeatureGate::ext(ServerFeatureExt::PixelSurface);
const TYPED_DOCUMENT: FeatureGate = FeatureGate::ext(ServerFeatureExt::TypedDocument);

/// The substrate every kind shares (L1 §1.1): spawn and close, the output
/// subscription, kill, events, inventory, and the resource's L3 scope.
pub static SUBSTRATE_METHODS: &[MethodSpec] = methods! {
    "SPAWN_RESOURCE" => Frame(TYPE_SPAWN_RESOURCE) [
        F_SPAWN_SATELLITE,
        F_SPAWN_LOCAL,
        F_SPAWN_OWNED,
        F_SPAWN_SATELLITE_OWNED,
        F_SPAWN_CHILD_LOCAL,
        F_SPAWN_CHILD_SATELLITE,
        F_SPAWN_KIND_MISMATCH,
        F_SPAWN_MACHINE_CONTROL,
        F_UNCLASSIFIED,
    ];
    "ATTACH_RESOURCE" => Command(COMMAND_TAG_ATTACH_RESOURCE) [C_ATTACH_RESOURCE];
    "DETACH_RESOURCE" => Command(COMMAND_TAG_DETACH_RESOURCE) [C_DETACH_RESOURCE];
    "KILL_RESOURCE" => Command(COMMAND_TAG_KILL_RESOURCE) [C_KILL_RESOURCE] dangerous;
    "KILL_RESOURCE_IF" => Command(COMMAND_TAG_KILL_RESOURCE_IF) [C_KILL_RESOURCE_IF]
        feature(ConditionalKill) dangerous;
    "KILL_RESOURCES" => Command(COMMAND_TAG_KILL_RESOURCES) [C_KILL_RESOURCES] dangerous;
    "CLOSE_TAB_RESOURCES" => Command(COMMAND_TAG_CLOSE_TAB_RESOURCES) [C_CLOSE_TAB_RESOURCES]
        feature(CloseTabResources) dangerous;
    "SUBSCRIBE_RESOURCE_EVENTS" => Command(COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS)
        [C_SUBSCRIBE_RESOURCE_EVENTS];
    "SUBSCRIBE_EVENTS" => Frame(TYPE_SUBSCRIBE_EVENTS)
        [F_SUBSCRIBE_EVENTS_ONE, F_SUBSCRIBE_EVENTS_ALL];
    "GET_STATE" => Command(COMMAND_TAG_GET_STATE) [C_GET_STATE];
    "GET_METADATA" => Frame(TYPE_GET_METADATA) [F_GET_METADATA, F_WHOAMI];
    "SET_METADATA" => Frame(TYPE_SET_METADATA) [
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
    ];
    "DELETE_METADATA" => Frame(TYPE_DELETE_METADATA)
        [F_METADATA_WRITE, F_RESULT_NAMESPACE_WRITE, F_SERVER_OWNED_WRITE];
    "LIST_METADATA" => Frame(TYPE_LIST_METADATA) [F_LIST_METADATA];
    "SUBSCRIBE_METADATA" => Frame(TYPE_SUBSCRIBE_METADATA)
        [F_SUBSCRIBE_METADATA, F_RESULT_NAMESPACE_SUBSCRIBE];
    // Replaces a document kind's whole state (an accessibility tree, a pixel
    // surface); the host routes it to the named resource's kind.
    "PUBLISH_RESOURCE_STATE" => Command(COMMAND_TAG_PUBLISH_RESOURCE_STATE)
        [C_PUBLISH_RESOURCE_STATE];
    // A viewer's input intent for a document, relayed to its producer.
    "SEND_DOCUMENT_INPUT" => Command(COMMAND_TAG_SEND_DOCUMENT_INPUT) [C_SEND_DOCUMENT_INPUT];
};

/// Events about the server or one subscription rather than one resource.
///
/// `journal_gap` tells a subscriber which journal `seq` range it missed; it
/// is never journaled itself (L1 §7.3). `approval_requested` and
/// `approval_decided` report a held `SIGNAL` action (ADR-0128). The
/// organization events (L1 §5.7) report committed changes to keyed records,
/// which outlive any one live resource.
pub static SERVER_EVENTS: &[EventSpec] = events! {
    "journal_gap" => EVENT_TAG_JOURNAL_GAP;
    "approval_requested" => EVENT_TAG_APPROVAL_REQUESTED;
    "approval_decided" => EVENT_TAG_APPROVAL_DECIDED;
    "session_created" => EVENT_TAG_SESSION_CREATED;
    "session_renamed" => EVENT_TAG_SESSION_RENAMED;
    "session_deleted" => EVENT_TAG_SESSION_DELETED;
    "session_lifetime_changed" => EVENT_TAG_SESSION_LIFETIME_CHANGED;
    "membership_changed" => EVENT_TAG_MEMBERSHIP_CHANGED;
    "resource_created" => EVENT_TAG_RESOURCE_CREATED;
    "resource_removed" => EVENT_TAG_RESOURCE_REMOVED;
    "activation_changed" => EVENT_TAG_ACTIVATION_CHANGED;
    "arrangement_changed" => EVENT_TAG_ARRANGEMENT_CHANGED;
    "lease_changed" => EVENT_TAG_LEASE_CHANGED;
    "intake_changed" => EVENT_TAG_INTAKE_CHANGED;
    "resource_renamed" => EVENT_TAG_RESOURCE_RENAMED;
    "session_project_set" => EVENT_TAG_SESSION_PROJECT_SET;
};

/// The events every kind shares: its own spawn and close, and `source_gap`
/// when the resource produced events faster than the server could journal
/// them (L1 §7.3).
pub static SUBSTRATE_EVENTS: &[EventSpec] = events! {
    "pane_spawned" => EVENT_TAG_RESOURCE_SPAWNED;
    "pane_closed" => EVENT_TAG_RESOURCE_CLOSED;
    "source_gap" => EVENT_TAG_SOURCE_GAP;
    "document_input" => EVENT_TAG_DOCUMENT_INPUT;
    "agent_request" => EVENT_TAG_AGENT_REQUEST;
};

/// Every resource kind this build serves, in wire-tag order.
pub static KINDS: [KindSpec; 5] = [
    KindSpec {
        kind: ResourceKind::Terminal,
        name: "TERMINAL",
        key: "terminal",
        gate: None,
        leaf: true,
        stream: StreamContract {
            codecs: &[
                StreamCodec {
                    tag: BootstrapCodec::SYNTHESIZED_VT_V1_TAG,
                    name: "SYNTHESIZED_VT_V1",
                    gate: None,
                },
                StreamCodec {
                    tag: BootstrapCodec::NATIVE_TAG,
                    name: "NATIVE",
                    gate: None,
                },
            ],
            delivery: &[Delivery::Raw, Delivery::Replicated],
            geometry: Geometry::Cells,
            resume: None,
            needs_terminal_profile: true,
        },
        spawn: SpawnContract {
            parent: ParentRule::Forbidden,
            provider: SpawnFacet {
                presence: Presence::Forbidden,
                meaning: "nothing: a Terminal runs its command",
                max_bytes: crate::wire::frame::MAX_RESOURCE_PROVIDER_BYTES,
            },
            native_id: SpawnFacet {
                presence: Presence::Forbidden,
                meaning: "nothing",
                max_bytes: crate::wire::frame::MAX_RESOURCE_NATIVE_ID_BYTES,
            },
            forbidden: &[spawn_field::CONTROL],
            validate: None,
            scoped_child: false,
        },
        methods: methods! {
            "INPUT_KEY" => Frame(TYPE_INPUT_KEY) [F_INPUT];
            "INPUT_PASTE" => Frame(TYPE_INPUT_PASTE) [F_INPUT];
            "INPUT_MOUSE" => Frame(TYPE_INPUT_MOUSE) [F_INPUT];
            "INPUT_FOCUS" => Frame(TYPE_INPUT_FOCUS) [F_INPUT];
            "INPUT_RAW" => Frame(INPUT_RAW_RESERVED_TYPE) [F_INPUT] unshipped;
            "INPUT_TERMINAL_REPLY" => Frame(TYPE_INPUT_TERMINAL_REPLY) [F_INPUT]
                feature(TerminalReply);
            "CLIPBOARD_REPLY" => Frame(TYPE_CLIPBOARD_REPLY) [F_INPUT] ext(ClipboardRead);
            "RESIZE_TERMINAL" => Frame(TYPE_RESIZE_TERMINAL) [F_RESIZE_TERMINAL];
            "HISTORY_REQUEST" => Frame(TYPE_HISTORY_REQUEST) [F_HISTORY_REQUEST];
            "FRAME_ACK" => Frame(TYPE_FRAME_ACK) [F_FRAME_ACK];
            "MOVE_RESOURCE" => Frame(TYPE_MOVE_RESOURCE) [F_MOVE_RESOURCE] feature(MoveResource);
            "GET_SCREEN" => Command(COMMAND_TAG_GET_SCREEN) [C_GET_SCREEN];
            "ROUTE_INPUT" => Command(COMMAND_TAG_ROUTE_INPUT) [C_INPUT];
            "APPLY_INPUT" => Command(COMMAND_TAG_APPLY_INPUT) [C_INPUT] feature(AcknowledgedInput);
            "PUT_FILE" => Command(COMMAND_TAG_PUT_FILE) [C_PUT_FILE] feature(FileUpload);
            "CHECKOUT" => Command(COMMAND_TAG_CHECKOUT) [C_CHECKOUT] ext(Checkout);
            "TRANSCRIBE" => Command(COMMAND_TAG_TRANSCRIBE) [C_TRANSCRIBE] feature(Transcribe);
            "GET_TERMINAL_STATE" => Command(COMMAND_TAG_GET_TERMINAL_STATE) [C_GET_TERMINAL_STATE];
            "ACQUIRE_INPUT" => Command(COMMAND_TAG_ACQUIRE_INPUT) [C_INPUT_LEASE];
            "RELEASE_INPUT" => Command(COMMAND_TAG_RELEASE_INPUT) [C_INPUT_LEASE];
            "SIGNAL_TERMINAL" => Command(COMMAND_TAG_SIGNAL_TERMINAL) [C_SIGNAL_TERMINAL] dangerous;
            "REPORT_ASKED" => Command(COMMAND_TAG_REPORT_ASKED) [C_AGENT_REPORT];
            "REPORT_AGENT_STATE" => Command(COMMAND_TAG_REPORT_AGENT_STATE) [C_AGENT_REPORT]
                feature(ReportAgentState);
        },
        events: events! {
            "command_started" => EVENT_TAG_COMMAND_STARTED;
            "command_finished" => EVENT_TAG_COMMAND_FINISHED;
            "title_changed" => EVENT_TAG_TITLE_CHANGED;
            "bell" => EVENT_TAG_BELL;
            "dirty" => EVENT_TAG_DIRTY;
            "idle" => EVENT_TAG_IDLE;
            "terminal_control" => EVENT_TAG_TERMINAL_CONTROL;
            "asked" => EVENT_TAG_ASKED;
            "cwd_changed" => EVENT_TAG_CWD_CHANGED;
            "input_activity" => EVENT_TAG_INPUT_ACTIVITY;
        },
        metadata_keys: &[
            RESOURCE_AGENT_KEY,
            RESOURCE_AGENT_SESSION_KEY,
            RESOURCE_PANE_OCCUPANT_KEY,
            crate::wire::frame::RESOURCE_CONTROLLER_KEY,
            RESOURCE_TAGS_KEY,
            RESOURCE_LINK_KEY,
            crate::wire::frame::RESOURCE_NOTE_KEY,
        ],
    },
    KindSpec {
        kind: ResourceKind::AgentSession,
        name: "AGENT_SESSION",
        key: "agent_session",
        gate: Some(FeatureGate::Features(ServerFeature::ResourceKinds)),
        // A record stream bound to its parent Terminal, never laid out.
        leaf: false,
        stream: StreamContract {
            codecs: &[StreamCodec {
                tag: BootstrapCodec::AGENT_EVENTS_JSONL_V1_TAG,
                name: "AGENT_EVENTS_JSONL_V1",
                gate: Some(FeatureGate::Features(ServerFeature::ResourceKinds)),
            }],
            delivery: &[Delivery::Log],
            geometry: Geometry::None,
            resume: Some(FeatureGate::ext(ServerFeatureExt::AgentStreamResume)),
            needs_terminal_profile: false,
        },
        spawn: projection_spawn(
            named_producer("the agent provider, e.g. claude"),
            "the provider's own session id",
            &AGENT_SESSION_REFUSED_FIELDS,
            None,
            true,
        ),
        methods: methods! {
            "APPEND_RESOURCE_OUTPUT" => Command(COMMAND_TAG_APPEND_RESOURCE_OUTPUT)
                [C_APPEND_RESOURCE_OUTPUT] feature(ResourceKinds);
            "NATIVE_AGENT" => Command(COMMAND_TAG_NATIVE_AGENT) [
                C_NATIVE_SPAWN,
                C_NATIVE_LIST,
                C_NATIVE_LAUNCH,
                C_NATIVE_COMMAND,
                C_NATIVE_CONTROLLER,
                C_CURRENT_QUESTIONS,
                C_CURRENT_QUESTIONS_KEYED,
                C_RESPOND_QUESTION,
                C_QUESTION_RECEIPT,
            ];
        },
        events: events! {
            "native_agent_result" => EVENT_TAG_NATIVE_AGENT_RESULT;
        },
        metadata_keys: &[],
    },
    KindSpec {
        kind: ResourceKind::AccessibilityTree,
        name: "ACCESSIBILITY_TREE",
        key: "accessibility_tree",
        gate: Some(ACCESSIBILITY_TREE),
        leaf: true,
        stream: whole_document_stream(
            &[StreamCodec {
                tag: BootstrapCodec::ACCESSIBILITY_TREE_JSON_V1_TAG,
                name: "ACCESSIBILITY_TREE_JSON_V1",
                gate: Some(ACCESSIBILITY_TREE),
            }],
            Geometry::None,
        ),
        spawn: projection_spawn(
            named_producer("the application the tree observes"),
            "the document's name",
            &PROJECTION_LAUNCH_FIELDS,
            None,
            true,
        ),
        // Fed through the substrate's `PUBLISH_RESOURCE_STATE`.
        methods: &[],
        events: &[],
        metadata_keys: &[],
    },
    KindSpec {
        kind: ResourceKind::PixelSurface,
        name: "PIXEL_SURFACE",
        key: "pixel_surface",
        gate: Some(PIXEL_SURFACE),
        leaf: true,
        stream: whole_document_stream(
            &[StreamCodec {
                tag: BootstrapCodec::PIXEL_SURFACE_PNG_V1_TAG,
                name: "PIXEL_SURFACE_PNG_V1",
                gate: Some(PIXEL_SURFACE),
            }],
            Geometry::Pixels,
        ),
        spawn: projection_spawn(
            named_producer("the window or display the surface shows"),
            "the document's name",
            &PROJECTION_LAUNCH_FIELDS,
            None,
            true,
        ),
        // Fed through the substrate's `PUBLISH_RESOURCE_STATE`.
        methods: &[],
        events: &[],
        metadata_keys: &[],
    },
    KindSpec {
        kind: ResourceKind::Document,
        name: "DOCUMENT",
        key: "document",
        gate: Some(TYPED_DOCUMENT),
        leaf: true,
        stream: whole_document_stream(
            &[StreamCodec {
                tag: BootstrapCodec::DOCUMENT_TYPED_V1_TAG,
                name: "DOCUMENT_TYPED_V1",
                gate: Some(TYPED_DOCUMENT),
            }],
            Geometry::None,
        ),
        spawn: projection_spawn(
            SpawnFacet {
                presence: Presence::Required,
                meaning: "the content type, a MIME type",
                max_bytes: crate::wire::frame::typed_document::MAX_CONTENT_TYPE_BYTES,
            },
            "the document's name",
            &PROJECTION_LAUNCH_FIELDS,
            Some(validate_document_spawn),
            true,
        ),
        // Fed through the substrate's `PUBLISH_RESOURCE_STATE`.
        methods: &[],
        events: &[],
        metadata_keys: &[],
    },
];
