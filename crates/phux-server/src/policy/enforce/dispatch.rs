/// Why a request was refused: verbs and subject kind, never the subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Denial {
    /// The verbs the refused operation needed; empty for a default-deny row.
    pub verbs: Verbs,
    /// `terminal`, `group`, `host`, `global`, `approval`, `unclassified`,
    /// `ungranted`, `expired`, `revoked`, `viewer`, or `held`.
    pub subject: &'static str,
}

impl Denial {
    /// A default-deny row: unknown, wrong-direction, retired, or a
    /// handshake frame after the handshake.
    const UNCLASSIFIED: Self = Self {
        verbs: Verbs::EMPTY,
        subject: "unclassified",
    };

    /// A connection with no minted grant. Fails closed.
    const UNGRANTED: Self = Self {
        verbs: Verbs::EMPTY,
        subject: "ungranted",
    };

    /// A grant past its credential's expiry.
    const EXPIRED: Self = Self {
        verbs: Verbs::EMPTY,
        subject: "expired",
    };

    /// A connection whose authority was withdrawn while it was live.
    const REVOKED: Self = Self {
        verbs: Verbs::EMPTY,
        subject: "revoked",
    };

    /// Input to a Terminal the connection subscribed as a `VIEWER`
    /// (ADR-0127).
    const VIEWER: Self = Self {
        verbs: Verbs::of(&[Verb::Input]),
        subject: "viewer",
    };
}

/// What the guard is asked to admit.
#[derive(Debug, Clone, Copy)]
pub enum Request<'a> {
    /// A decoded client frame other than `COMMAND`.
    Frame(&'a FrameKind),
    /// A nested command, classified by its own tag.
    Command(&'a Command),
    /// A QUIC `STREAM_BIND` for a Terminal: `OBSERVE` on it.
    StreamBind(&'a WireResourceId),
}

/// Guard one decoded frame for `client`. A frame is never held: a held
/// `SIGNAL` on one is refused (ADR-0128).
pub fn authorize_frame(s: &ServerState, client: ClientId, frame: &FrameKind) -> Result<(), Denial> {
    authorize(s, client, Request::Frame(frame)).map(drop)
}

/// Guard one nested command before any handler or relay: run it, or hold
/// it for a decision (ADR-0128).
pub fn authorize_command(
    s: &ServerState,
    client: ClientId,
    command: &Command,
) -> Result<Admission, Denial> {
    authorize(s, client, Request::Command(command))
}

/// Guard one QUIC Terminal-stream bind for `client`.
pub fn authorize_stream_bind(
    s: &ServerState,
    client: ClientId,
    terminal: &WireResourceId,
) -> Result<(), Denial> {
    authorize(s, client, Request::StreamBind(terminal)).map(drop)
}

fn authorize(s: &ServerState, client: ClientId, request: Request<'_>) -> Result<Admission, Denial> {
    let Some(grant) = s.connection_grant(client) else {
        tracing::debug!(target: POLICY_TARGET, ?client, "operation denied: no grant was minted");
        return Err(Denial::UNGRANTED);
    };
    enforce(s, client, grant, request)
        .and_then(|()| refuse_viewer_input(s, client, request))
        .and_then(|()| super::hold::refuse_viewer_decision(s, client, request))
        .and_then(|()| super::hold::admission(s, client, grant, request))
        .inspect_err(|denial| trace_denial(grant, *denial))
}

/// A `VIEWER` subscription is observe-only (ADR-0127): input or lease
/// requests on such a Terminal are refused whatever the grant admits.
fn refuse_viewer_input(
    s: &ServerState,
    client: ClientId,
    request: Request<'_>,
) -> Result<(), Denial> {
    let request = unwrap_command(request);
    if matches!(request, Request::Command(Command::NativeAgent { request })
        if matches!(request.request(), phux_protocol::native_agent::NativeRequest::LaunchIntegration { .. }))
        && s.is_attached_viewer(client)
    {
        return Err(Denial::VIEWER);
    }
    let Some(terminal) = named_terminal(request) else {
        return Ok(());
    };
    if is_input_request(request) && s.is_viewer(client, terminal) {
        return Err(Denial::VIEWER);
    }
    Ok(())
}

/// `ACQUIRE_INPUT`, or any row that needs `INPUT` on a named Terminal:
/// `INPUT_*`, `ROUTE_INPUT`, `APPLY_INPUT`, `PUT_FILE`, `TRANSCRIBE`.
fn is_input_request(request: Request<'_>) -> bool {
    if matches!(request, Request::Command(Command::AcquireInput { .. })) {
        return true;
    }
    matches!(
        classify(request),
        Classification::Allow { verbs, subject: Subject::NamedTerminal }
            if verbs.iter().any(|verb| verb == Verb::Input)
    )
}

/// Decide `request` for `client` under `grant` against the handler's state
/// snapshot. The owner's grant admits everything; a scoped grant is checked
/// against the request's §6 row.
///
/// # Errors
///
/// A [`Denial`] for a default-deny row, an unmet transport predicate, or a
/// missing verb on the resolved subject.
pub fn enforce(
    s: &ServerState,
    client: ClientId,
    grant: &ConnectionGrant,
    request: Request<'_>,
) -> Result<(), Denial> {
    // Revocation first (§7 step 1): it stops owner grants too.
    if grant.revocation().is_some() {
        return Err(Denial::REVOKED);
    }
    let Authority::Scoped { effective, .. } = &grant.authority else {
        return Ok(());
    };
    // Expiry is a policy time bound to the grant (workload-auth §5): past
    // it, nothing is admitted, whether or not the connection is still open.
    if grant.is_expired_at(chrono::Utc::now()) {
        return Err(Denial::EXPIRED);
    }
    let request = unwrap_command(request);
    match classify(request) {
        // HELLO is valid only before the handshake, and the guard runs after.
        Classification::Deny | Classification::Exempt(Exemption::Handshake) => {
            Err(Denial::UNCLASSIFIED)
        }
        Classification::Exempt(Exemption::Liveness | Exemption::Cleanup | Exemption::SelfRead) => {
            Ok(())
        }
        Classification::Allow { verbs, subject } => {
            // One denial per row, whatever failed: an absent target and an
            // unauthorized one are refused identically.
            let refused = Denial {
                verbs,
                subject: subject_kind(subject),
            };
            let needs = needs_for(s, client, subject, verbs, request).ok_or(refused)?;
            if covers_all(effective, &needs) && frame_ack_is_current(s, client, request) {
                Ok(())
            } else {
                Err(refused)
            }
        }
    }
}

/// A `COMMAND` frame is classified, and its subject resolved, by the nested
/// command.
pub(super) const fn unwrap_command(request: Request<'_>) -> Request<'_> {
    match request {
        Request::Frame(FrameKind::Command { command, .. }) => Request::Command(command),
        other => other,
    }
}

pub(super) fn classify(request: Request<'_>) -> Classification {
    match request {
        Request::Frame(frame) => classify_frame(frame),
        Request::Command(command) => classify_command(command),
        Request::StreamBind(_) => Classification::Allow {
            verbs: OBSERVE,
            subject: Subject::NamedTerminal,
        },
    }
}

fn trace_denial(grant: &ConnectionGrant, denial: Denial) {
    let verbs: Vec<&str> = denial.verbs.iter().map(Verb::name).collect();
    tracing::debug!(
        target: POLICY_TARGET,
        credential = grant.credential_id.as_deref().unwrap_or("-"),
        verbs = %verbs.join("+"),
        subject = denial.subject,
        "operation denied",
    );
}

// --- Needs: one verb set on one subject; all must pass ---

pub(super) struct Need {
    verbs: Verbs,
    point: Point,
}

impl Need {
    const fn new(verbs: Verbs, point: Point) -> Self {
        Self { verbs, point }
    }
}

/// Whether every verb of every need is granted on its subject by some clause
/// whose two selectors both contain it.
pub(super) fn covers_all(effective: &EffectiveScopeSet, needs: &[Need]) -> bool {
    needs.iter().all(|need| {
        need.verbs
            .iter()
            .all(|verb| effective.admits(verb, |selector| contains(selector, &need.point)))
    })
}

/// The needs one §6 row places on `request`, or `None` when the subject
/// cannot be resolved: that is a refusal, never a skip.
pub(super) fn needs_for(
    s: &ServerState,
    client: ClientId,
    subject: Subject,
    verbs: Verbs,
    request: Request<'_>,
) -> Option<Vec<Need>> {
    let one = |point: Point| vec![Need::new(verbs, point)];
    match subject {
        Subject::NamedTerminal => named_terminal(request).map(|id| one(terminal_subject(s, id))),
        Subject::EveryNamedTerminal => Some(every_named(s, verbs, request)),
        Subject::AttachedTerminals => Some(attached_terminals(s, client, verbs)),
        Subject::MovedAndOwnerTerminals => moved_and_owner(s, verbs, request),
        Subject::ResolvedGroup | Subject::SelectedLocalGroup | Subject::NamedSession => {
            resolved_group(s, request).map(one)
        }
        // An unowned spawn lands in a session the server picks.
        Subject::PayloadGroup => Some(one(Point::LocalHost)),
        Subject::SatelliteHost => {
            spawn_satellite(request).map(|host| one(Point::SatelliteHost(host)))
        }
        Subject::OwnerTerminalGroup => {
            spawn_owner(request).and_then(|owner| create_beside(s, owner))
        }
        Subject::ParentTerminalGroup => {
            spawn_parent(request).and_then(|parent| create_beside(s, parent))
        }
        Subject::SatelliteHostAndParent => satellite_agent_spawn(request),
        Subject::ParentOfNamed => named_terminal(request)
            .and_then(|id| parent_subject(s, id))
            .map(one),
        Subject::MetadataScope => metadata_scope(request).map(|scope| one(scope_subject(s, scope))),
        Subject::HeldAction => super::hold::held_action_needs(s, request),
        // A filtered result needs its verb somewhere; what it returns is
        // filtered at the source (`super::filter`).
        // An organization page is filtered record by record the same way.
        Subject::ObservableTerminals | Subject::InventoryMatches | Subject::OrganizationView => {
            Some(one(Point::Anywhere))
        }
        Subject::KeyedSession => keyed_session(s, request).map(one),
        Subject::KeyedResource => keyed_resource(s, request).map(one),
        Subject::MoveSessions => move_sessions(s, verbs, request),
        Subject::UndoneOperation => undone_operation(s, verbs, request),
        Subject::ArrangementOwner => arrangement_owner(s, request).map(one),
        Subject::Global {
            owner_uds_only: false,
        } => Some(one(Point::Global)),
        // A scoped grant never rides the owner's socket; and rows without a
        // subject carry no verbs to admit.
        Subject::Global {
            owner_uds_only: true,
        }
        | Subject::None
        | Subject::CallingConnection => None,
    }
}

const fn subject_kind(subject: Subject) -> &'static str {
    match subject {
        Subject::NamedTerminal
        | Subject::EveryNamedTerminal
        | Subject::AttachedTerminals
        | Subject::MovedAndOwnerTerminals
        | Subject::ParentOfNamed
        | Subject::KeyedResource => "terminal",
        Subject::ObservableTerminals | Subject::InventoryMatches | Subject::OrganizationView => {
            "any"
        }
        Subject::KeyedSession
        | Subject::MoveSessions
        | Subject::UndoneOperation
        | Subject::ArrangementOwner
        | Subject::ResolvedGroup
        | Subject::SelectedLocalGroup
        | Subject::NamedSession
        | Subject::PayloadGroup
        | Subject::OwnerTerminalGroup
        | Subject::ParentTerminalGroup => "group",
        Subject::SatelliteHost | Subject::SatelliteHostAndParent => "host",
        Subject::MetadataScope | Subject::Global { .. } => "global",
        Subject::HeldAction => "approval",
        Subject::None | Subject::CallingConnection => "unclassified",
    }
}

/// `FRAME_ACK` is admitted only from a subscriber of that local Terminal
/// (the actor drops stale generations); satellite acks are relayed.
fn frame_ack_is_current(s: &ServerState, client: ClientId, request: Request<'_>) -> bool {
    let Request::Frame(FrameKind::FrameAck { terminal_id, .. }) = request else {
        return true;
    };
    if !terminal_id.is_local() {
        return true;
    }
    s.terminal_from_wire(terminal_id)
        .is_some_and(|core| s.subscribers_for_terminal(core).contains(&client))
}
