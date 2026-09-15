//! The dispatch guard: one [`enforce`] behind the three entry points the
//! client loop calls (`docs/spec/workload-auth.md` §6, §7).
//!
//! The classification is [`phux_protocol::kinds`], full stop. This module
//! only resolves each row's subject against server state, side-effect-free
//! and under the caller's state borrow, and checks the connection's
//! conjunctive clauses against it. Absent and unauthorized targets resolve
//! to subjects no selector the connection holds contains, so they are
//! refused identically.
//!
//! Group means a session here: the resolved Group of an `ATTACH` or a
//! forced detach is a session, and a Terminal's current Group is the session
//! whose window holds it, named by its wire session id. `GroupId` (the
//! `SPAWN_RESOURCE` payload group and the `Scope::Group` metadata key) is an
//! opaque grouping key the server serves only as `GroupId(1)` (L1 §3.1,
//! L2 §3), so it resolves to the local host as a whole.

use phux_core::ids::ResourceId as CoreResourceId;
use phux_protocol::ids::{ResourceId as WireResourceId, SessionId as WireSessionId};
use phux_protocol::kinds::{
    Classification, Exemption, Subject, Verb, Verbs, classify_command, classify_frame,
};
use phux_protocol::scope::{EffectiveScopeSet, Selector};
use phux_protocol::wire::frame::{
    AttachTarget, Command, FrameKind, Scope, decode_session_keep_empty,
};

use super::{Authority, ConnectionGrant, POLICY_TARGET};
use crate::state::{ClientId, ServerState};

const OBSERVE: Verbs = Verbs::of(&[Verb::Observe]);
const CREATE: Verbs = Verbs::of(&[Verb::Create]);
const BIND: Verbs = Verbs::of(&[Verb::Bind]);

/// Why a frame, command, or stream bind was refused.
///
/// It names the verbs and the *kind* of subject, never the subject itself,
/// so the tracing line it becomes cannot disclose topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Denial {
    /// The verbs the refused operation needed; empty for a default-deny row.
    pub verbs: Verbs,
    /// `terminal`, `group`, `host`, `global`, `unclassified`, `ungranted`,
    /// `expired`, or `revoked`.
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

/// Guard one decoded frame for `client`.
pub fn authorize_frame(s: &ServerState, client: ClientId, frame: &FrameKind) -> Result<(), Denial> {
    authorize(s, client, Request::Frame(frame))
}

/// Guard one nested command for `client`, before any handler, input-lane
/// route, or satellite relay.
pub fn authorize_command(
    s: &ServerState,
    client: ClientId,
    command: &Command,
) -> Result<(), Denial> {
    authorize(s, client, Request::Command(command))
}

/// Guard one QUIC Terminal-stream bind for `client`.
pub fn authorize_stream_bind(
    s: &ServerState,
    client: ClientId,
    terminal: &WireResourceId,
) -> Result<(), Denial> {
    authorize(s, client, Request::StreamBind(terminal))
}

fn authorize(s: &ServerState, client: ClientId, request: Request<'_>) -> Result<(), Denial> {
    let Some(grant) = s.connection_grant(client) else {
        tracing::debug!(target: POLICY_TARGET, ?client, "operation denied: no grant was minted");
        return Err(Denial::UNGRANTED);
    };
    enforce(s, client, grant, request)
        .and_then(|()| refuse_viewer_input(s, client, request))
        .inspect_err(|denial| trace_denial(grant, *denial))
}

/// A `VIEWER` subscription is observe-only (ADR-0127): a request that needs
/// `INPUT` on a Terminal the connection subscribed as a viewer, or asks for
/// its lease, is refused whatever the grant admits, through the same
/// refusal paths a scope denial takes. The role is intent the connection
/// declared; widening it takes a fresh `ATTACH_RESOURCE { PRIMARY }`, which
/// is journaled.
fn refuse_viewer_input(
    s: &ServerState,
    client: ClientId,
    request: Request<'_>,
) -> Result<(), Denial> {
    let request = unwrap_command(request);
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

/// Decide `request` for `client` under `grant`, reading `s` as the snapshot
/// the handler will route with.
///
/// The owner's grant admits everything: its handlers and their domain checks
/// are the whole policy, as before enforcement existed. A scoped grant is
/// checked against the request's §6 row.
///
/// # Errors
///
/// A [`Denial`] when the row is default-deny, carries a transport predicate
/// a scoped grant cannot meet, or needs a verb no clause grants on the
/// resolved subject.
pub fn enforce(
    s: &ServerState,
    client: ClientId,
    grant: &ConnectionGrant,
    request: Request<'_>,
) -> Result<(), Denial> {
    // A revoked connection admits nothing, whatever shape its authority had
    // (workload-auth §7 step 1). Checked before the owner's shortcut: a
    // bearer-admitted connection holds the owner's grant in the transitional
    // posture, and its revocation must still stop it.
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
const fn unwrap_command(request: Request<'_>) -> Request<'_> {
    match request {
        Request::Frame(FrameKind::Command { command, .. }) => Request::Command(command),
        other => other,
    }
}

fn classify(request: Request<'_>) -> Classification {
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

// -----------------------------------------------------------------------------
// Needs: one verb set on one resolved subject; every need must pass.
// -----------------------------------------------------------------------------

struct Need {
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
fn covers_all(effective: &EffectiveScopeSet, needs: &[Need]) -> bool {
    needs.iter().all(|need| {
        need.verbs
            .iter()
            .all(|verb| effective.admits(verb, |selector| contains(selector, &need.point)))
    })
}

/// The needs one §6 row places on `request`, or `None` when the subject
/// cannot be resolved: that is a refusal, never a skip.
fn needs_for(
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
        // The filtered result `ObservableTerminals` and `InventoryMatches`
        // allow is not built: they require Global, which the unfiltered
        // result is.
        Subject::ObservableTerminals
        | Subject::InventoryMatches
        | Subject::Global {
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
        | Subject::ParentOfNamed => "terminal",
        Subject::ResolvedGroup
        | Subject::SelectedLocalGroup
        | Subject::NamedSession
        | Subject::PayloadGroup
        | Subject::OwnerTerminalGroup
        | Subject::ParentTerminalGroup => "group",
        Subject::SatelliteHost | Subject::SatelliteHostAndParent => "host",
        Subject::ObservableTerminals
        | Subject::InventoryMatches
        | Subject::MetadataScope
        | Subject::Global { .. } => "global",
        Subject::None | Subject::CallingConnection => "unclassified",
    }
}

/// `FRAME_ACK` names the Terminal *and* its current stream generation. The
/// guard admits it only from a connection subscribed to that local Terminal;
/// the Terminal's actor then drops an ack for any stream or bootstrap
/// generation other than the current one. A satellite Terminal's ack is
/// relayed, and its actor on the satellite holds the generation.
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

// -----------------------------------------------------------------------------
// Resolved subjects and containment.
// -----------------------------------------------------------------------------

/// A resolved subject.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Point {
    /// Every subject: server-global data.
    Global,
    /// A local subject no narrower selector names: a Group that does not
    /// exist (yet or any more), the opaque `GroupId` scope key, or the
    /// session an unowned spawn lands in. Only Global and the local Host
    /// contain it.
    LocalHost,
    /// A local Group (session), by wire session id.
    Group(u32),
    /// A satellite host.
    SatelliteHost(String),
    /// A Terminal, or a child resource admitted through its parent.
    Terminal(TerminalPoint),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalPoint {
    at: Located,
    /// The wire session id of the window holding it; `None` when it is
    /// absent, a satellite Terminal, or a child with no window.
    group: Option<u32>,
    /// A child resource's parent Terminal: the child matches whatever its
    /// parent matches (workload-auth §6, L1 §1.2).
    parent: Option<Box<Self>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Located {
    Local(u32),
    /// A local resource no wire id names yet. No client can have addressed
    /// it, so only Global, the local Host, and its Group contain it.
    Unnamed,
    Satellite(String, u32),
}

impl TerminalPoint {
    fn of(s: &ServerState, wire: &WireResourceId) -> Self {
        match wire {
            WireResourceId::Local { id } => local_terminal(s, *id, s.terminal_from_wire(wire)),
            WireResourceId::Satellite { host, id } => Self {
                at: Located::Satellite(host.as_str().to_owned(), *id),
                group: None,
                parent: None,
            },
        }
    }

    /// The same Terminal, matched only as itself.
    fn alone(self) -> Self {
        Self {
            parent: None,
            ..self
        }
    }

    /// The local Group holding it, if it is a local Terminal in a window.
    const fn local_group(&self) -> Option<u32> {
        match self.at {
            Located::Local(_) | Located::Unnamed => self.group,
            Located::Satellite(..) => None,
        }
    }
}

fn local_terminal(s: &ServerState, id: u32, core: Option<CoreResourceId>) -> TerminalPoint {
    TerminalPoint {
        at: Located::Local(id),
        group: core.and_then(|core| group_of(s, core)),
        parent: core
            .and_then(|core| s.resource_parent(core))
            .map(|parent| Box::new(local_point(s, parent))),
    }
}

/// A local resource by core id, matched only as itself. One with no wire id
/// yet is [`Located::Unnamed`], never skipped.
fn local_point(s: &ServerState, core: CoreResourceId) -> TerminalPoint {
    let at = match s.terminal_wire_of(core) {
        Some(WireResourceId::Local { id }) => Located::Local(id),
        _ => Located::Unnamed,
    };
    TerminalPoint {
        at,
        group: group_of(s, core),
        parent: None,
    }
}

/// The wire session id of the session whose window holds `core`.
fn group_of(s: &ServerState, core: CoreResourceId) -> Option<u32> {
    let window = s.registry().resource(core)?.window?;
    let session = s.registry().window(window)?.session;
    s.idspace.session_wire(session).map(WireSessionId::get)
}

fn terminal_subject(s: &ServerState, wire: &WireResourceId) -> Point {
    Point::Terminal(TerminalPoint::of(s, wire))
}

/// `APPEND_RESOURCE_OUTPUT` is admitted through the named resource's parent
/// alone; a grant naming only the child does not suffice. A local resource
/// with no parent (a Terminal, or an absent id) is its own subject, and the
/// handler then refuses it. A satellite child's parent is not known here, so
/// it fails closed.
fn parent_subject(s: &ServerState, wire: &WireResourceId) -> Option<Point> {
    let WireResourceId::Local { id } = wire else {
        return None;
    };
    let core = s.terminal_from_wire(wire);
    let subject = core.and_then(|core| s.resource_parent(core)).map_or_else(
        || local_terminal(s, *id, core).alone(),
        |parent| local_point(s, parent),
    );
    Some(Point::Terminal(subject))
}

fn scope_subject(s: &ServerState, scope: &Scope) -> Point {
    match scope {
        Scope::Resource(id) => terminal_subject(s, id),
        Scope::Group(_) => Point::LocalHost,
        // Global, and any scope a later protocol adds: only a Global grant
        // covers it, which fails closed.
        _ => Point::Global,
    }
}

/// Whether `selector` contains `point`. A child resource is contained when
/// either it or its parent is.
fn contains(selector: &Selector, point: &Point) -> bool {
    match point {
        Point::Terminal(terminal) => {
            terminal_in(selector, terminal)
                || terminal
                    .parent
                    .as_deref()
                    .is_some_and(|parent| terminal_in(selector, parent))
        }
        place => place_in(selector, place),
    }
}

fn place_in(selector: &Selector, point: &Point) -> bool {
    match (selector, point) {
        (Selector::Global, _) | (Selector::HostLocal, Point::LocalHost | Point::Group(_)) => true,
        (Selector::HostSatellite(host), Point::SatelliteHost(name)) => host.as_str() == name,
        (Selector::Group(id), Point::Group(group)) => id == group,
        _ => false,
    }
}

fn terminal_in(selector: &Selector, terminal: &TerminalPoint) -> bool {
    match (selector, &terminal.at) {
        (Selector::Global, _) | (Selector::HostLocal, Located::Local(_) | Located::Unnamed) => true,
        (Selector::HostSatellite(host), Located::Satellite(name, _)) => host.as_str() == name,
        (Selector::Group(id), Located::Local(_) | Located::Unnamed) => terminal.group == Some(*id),
        (Selector::TerminalLocal(id), Located::Local(at)) => id == at,
        (Selector::TerminalSatellite(host, id), Located::Satellite(name, at)) => {
            host.as_str() == name && id == at
        }
        _ => false,
    }
}

// -----------------------------------------------------------------------------
// Subject extraction, per row.
// -----------------------------------------------------------------------------

/// The Terminal a "named Terminal" row names.
const fn named_terminal(request: Request<'_>) -> Option<&WireResourceId> {
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

const fn command_terminal(command: &Command) -> Option<&WireResourceId> {
    match command {
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
        | Command::Transcribe { terminal_id, .. }
        | Command::AppendResourceOutput { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

/// `KILL_RESOURCES` / `CLOSE_TAB_RESOURCES`: every named Terminal, all-or-nothing.
/// Zero targets is a no-op, so it needs nothing.
fn every_named(s: &ServerState, verbs: Verbs, request: Request<'_>) -> Vec<Need> {
    let ids = match request {
        Request::Command(Command::KillResources { ids } | Command::CloseTabResources { ids }) => {
            ids
        }
        _ => return Vec::new(),
    };
    ids.iter()
        .map(|id| Need::new(verbs, terminal_subject(s, id)))
        .collect()
}

/// `VIEWPORT_RESIZE`: every Terminal of the session the connection is
/// attached to, each one checked, none skipped. Zero targets is a no-op.
fn attached_terminals(s: &ServerState, client: ClientId, verbs: Verbs) -> Vec<Need> {
    let Some(session) = s.attached().get(&client).map(|attached| attached.session) else {
        return Vec::new();
    };
    let registry = s.registry();
    let windows = registry
        .session(session)
        .map(|session| session.windows.clone())
        .unwrap_or_default();
    windows
        .iter()
        .filter_map(|window| registry.window(*window))
        .flat_map(|window| window.slots.iter().copied())
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

/// Side-effect-free mirror of `resolve_attach_target`: nothing is created,
/// and a create-if-missing target names the session it would reuse.
///
/// The attach handler pins this id before its first await and attaches to it,
/// so the session the guard authorized is the session the client joins.
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
        .and_then(|session| s.idspace.session_wire(session))
        .map_or(Point::LocalHost, |wire| Point::Group(wire.get()))
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

/// `CREATE` on the anchor Terminal's resolved Group and `BIND` on the anchor
/// itself: owner-addressed spawn, and a local agent session's parent. An
/// anchor with no local Group (absent, satellite, windowless) fails closed.
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
