// --- Resolved subjects and containment ---

/// A resolved subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Point {
    /// Every subject: server-global data.
    Global,
    /// Whatever the grant covers: a row whose result is filtered at the
    /// source needs its verb on some selector, never on a particular one.
    Anywhere,
    /// A local subject no narrower selector names (absent Group, the opaque
    /// `GroupId`, an unowned spawn's session); only Global and Host contain
    /// it.
    LocalHost,
    /// A local Group (session), by wire session id and record key.
    Group(GroupPoint),
    /// A satellite host.
    SatelliteHost(String),
    /// A Terminal, or a child resource admitted through its parent.
    Terminal(TerminalPoint),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalPoint {
    pub(super) at: Located,
    /// The session holding it; `None` when it is absent, a satellite
    /// Terminal, or a child with no window.
    pub(super) group: Option<GroupPoint>,
    /// Its resource record's persistent key, which a `resource:` selector
    /// names; `None` for a satellite Terminal or one with no record.
    pub(super) key: Option<phux_protocol::ids::ResourceKey>,
    /// A child resource's parent Terminal: the child matches whatever its
    /// parent matches (workload-auth §6, L1 §1.2).
    pub(super) parent: Option<Box<Self>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Located {
    Local(u32),
    /// A local resource no wire id names yet. No client can have addressed
    /// it, so only Global, the local Host, its Group (by id or `session:`
    /// key), and its record's `resource:` key contain it.
    Unnamed,
    Satellite(String, u32),
}

impl TerminalPoint {
    pub(super) fn of(s: &ServerState, wire: &WireResourceId) -> Self {
        match wire {
            WireResourceId::Local { id } => local_terminal(s, *id, s.terminal_from_wire(wire)),
            WireResourceId::Satellite { host, id } => Self {
                at: Located::Satellite(host.as_str().to_owned(), *id),
                group: None,
                key: None,
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
    const fn local_group(&self) -> Option<GroupPoint> {
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
        key: core.and_then(|core| resource_key_of(s, core)),
        parent: core
            .and_then(|core| s.resource_parent(core))
            .map(|parent| Box::new(local_point(s, parent))),
    }
}

/// A local resource by core id, matched only as itself. One with no wire id
/// yet is [`Located::Unnamed`], never skipped.
pub(super) fn local_point(s: &ServerState, core: CoreResourceId) -> TerminalPoint {
    let at = match s.terminal_wire_of(core) {
        Some(WireResourceId::Local { id }) => Located::Local(id),
        _ => Located::Unnamed,
    };
    TerminalPoint {
        at,
        group: group_of(s, core),
        key: resource_key_of(s, core),
        parent: None,
    }
}

/// The session `core` is a member of now, so a membership move changes what
/// a `group:` or `session:` selector reaches in the same commit.
pub(super) fn group_of(s: &ServerState, core: CoreResourceId) -> Option<GroupPoint> {
    s.registry()
        .session_of(core)
        .and_then(|session| GroupPoint::of(s, session))
}

/// The persistent key of the resource record behind `core`.
pub(super) fn resource_key_of(
    s: &ServerState,
    core: CoreResourceId,
) -> Option<phux_protocol::ids::ResourceKey> {
    s.registry()
        .resource_key(core)
        .and_then(crate::org_wire::resource_key)
}

fn terminal_subject(s: &ServerState, wire: &WireResourceId) -> Point {
    Point::Terminal(TerminalPoint::of(s, wire))
}

/// `APPEND_RESOURCE_OUTPUT` is admitted through the parent only; a
/// parentless local resource is its own subject; a satellite child fails
/// closed.
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
pub(super) fn contains(selector: &Selector, point: &Point) -> bool {
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
        (Selector::Global, _)
        | (_, Point::Anywhere)
        | (Selector::HostLocal, Point::LocalHost | Point::Group(_)) => true,
        (Selector::HostSatellite(host), Point::SatelliteHost(name)) => host.as_str() == name,
        (Selector::Group(_) | Selector::Session(_), Point::Group(group)) => group.is_in(selector),
        _ => false,
    }
}

fn terminal_in(selector: &Selector, terminal: &TerminalPoint) -> bool {
    match (selector, &terminal.at) {
        (Selector::Global, _) | (Selector::HostLocal, Located::Local(_) | Located::Unnamed) => true,
        (Selector::HostSatellite(host), Located::Satellite(name, _)) => host.as_str() == name,
        (Selector::Group(_) | Selector::Session(_), Located::Local(_) | Located::Unnamed) => {
            terminal.group.is_some_and(|group| group.is_in(selector))
        }
        (Selector::TerminalLocal(id), Located::Local(at)) => id == at,
        (Selector::Resource(key), Located::Local(_) | Located::Unnamed) => {
            terminal.key == Some(*key)
        }
        (Selector::TerminalSatellite(host, id), Located::Satellite(name, at)) => {
            host.as_str() == name && id == at
        }
        _ => false,
    }
}
