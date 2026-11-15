/// One `AgentEvent` variant (L1 §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventSpec {
    /// The spec's name for the event, e.g. `cwd_changed`.
    pub name: &'static str,
    /// The event's wire tag inside `EVENT`.
    pub tag: u8,
}

/// One resource kind and its facet (L1 §1.1): the methods, events, and
/// resource-scoped metadata keys only that kind answers, and the contracts
/// its stream and its spawn keep.
#[derive(Debug, Clone, Copy)]
pub struct KindSpec {
    /// The kind.
    pub kind: ResourceKind,
    /// The spec's name for the kind, e.g. `TERMINAL`.
    pub name: &'static str,
    /// The lower-case stable name, e.g. `agent_session`
    /// ([`ResourceKind::as_str`]).
    pub key: &'static str,
    /// The feature bit that says the server serves this kind, if any.
    pub gate: Option<FeatureGate>,
    /// Whether a resource of this kind can occupy an arrangement leaf
    /// ([`ResourceKind::can_occupy_leaf`]).
    pub leaf: bool,
    /// Its bootstrap stream.
    pub stream: StreamContract,
    /// Its `SPAWN_RESOURCE` field rules.
    pub spawn: SpawnContract,
    /// The facet methods.
    pub methods: &'static [MethodSpec],
    /// The facet events.
    pub events: &'static [EventSpec],
    /// Conventional metadata keys scoped to a resource of this kind.
    pub metadata_keys: &'static [&'static str],
}
