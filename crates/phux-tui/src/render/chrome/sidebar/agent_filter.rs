//! Which agents the sidebar's Agents panel lists: a scope (every agent, this
//! session's, or only the ones that need you) crossed with a machine.
//!
//! The panel paints the filter as a chip row under its header; one click on
//! a chip picks that scope and a click on the `@` chip steps through the
//! machines that have agents. `agents-filter` drives the same state from a
//! key or the palette. [`filter_chips`] is the one layout both the painter
//! and the hit test read, so a chip's paint and its click target agree.

use super::AgentEntry;

/// Which agents the panel admits, before the machine filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentScope {
    /// Every agent this client can see, on every session.
    #[default]
    All,
    /// The attached session's agents.
    Session,
    /// Only the agents that need a human (the attention ladder's top rungs).
    Attention,
}

impl AgentScope {
    /// Every scope, in chip and cycle order.
    pub const ALL: [Self; 3] = [Self::All, Self::Session, Self::Attention];

    /// The chip label: short enough that all three fit a 20-column strip.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Session => "this",
            Self::Attention => "need",
        }
    }

    /// The `agents-filter { scope }` argument naming this scope.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Session => "session",
            Self::Attention => "attention",
        }
    }

    /// The scope an `agents-filter { scope }` argument names. Accepts the
    /// chip labels too, so what the strip shows is what a binding can say.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "session" | "this" => Some(Self::Session),
            "attention" | "need" | "needs-you" => Some(Self::Attention),
            _ => None,
        }
    }

    /// The scope after this one, wrapping.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::All => Self::Session,
            Self::Session => Self::Attention,
            Self::Attention => Self::All,
        }
    }

    const fn admits(self, entry: &AgentEntry) -> bool {
        match self {
            Self::All => true,
            Self::Session => entry.session.is_none(),
            Self::Attention => entry.needs_human(),
        }
    }
}

/// Which machine's agents the panel admits.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HostFilter {
    /// Every machine.
    #[default]
    Any,
    /// The machine this client is attached to (rows with no host).
    Here,
    /// One satellite host, by the name its rows carry.
    Host(String),
}

impl HostFilter {
    fn admits(&self, entry: &AgentEntry) -> bool {
        match self {
            Self::Any => true,
            Self::Here => entry.host.is_none(),
            Self::Host(name) => entry.host.as_deref() == Some(name.as_str()),
        }
    }

    /// The `@` chip's label.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Any => "@all".to_owned(),
            Self::Here => "@here".to_owned(),
            Self::Host(name) => format!("@{name}"),
        }
    }

    /// The `agents-filter { host }` argument naming this filter.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Any => "all",
            Self::Here => "here",
            Self::Host(name) => name,
        }
    }

    /// The filter an `agents-filter { host }` argument names: `all`, `here`,
    /// or a host name.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "all" | "" => Self::Any,
            "here" | "local" => Self::Here,
            name => Self::Host(name.to_owned()),
        }
    }

    /// The machine after this one among `hosts` (the satellite names that
    /// have agents, in first-seen order): every machine, here, each
    /// satellite, then every machine again. A host that vanished restarts
    /// the cycle.
    #[must_use]
    pub fn next(&self, hosts: &[String]) -> Self {
        match self {
            Self::Any => Self::Here,
            Self::Here => hosts.first().cloned().map_or(Self::Any, Self::Host),
            Self::Host(name) => hosts
                .iter()
                .position(|host| host == name)
                .and_then(|at| hosts.get(at + 1))
                .cloned()
                .map_or(Self::Any, Self::Host),
        }
    }
}

/// The Agents panel's filter: a scope crossed with a machine.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentFilter {
    /// Which agents.
    pub scope: AgentScope,
    /// Whose machine.
    pub host: HostFilter,
}

impl AgentFilter {
    /// Whether the panel lists `entry`.
    #[must_use]
    pub fn admits(&self, entry: &AgentEntry) -> bool {
        self.scope.admits(entry) && self.host.admits(entry)
    }
}

/// An `agents-filter` request: each half sets its part of the filter or
/// steps it, and an absent half leaves it alone. Bare `agents-filter` steps
/// the scope.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterRequest {
    /// The scope to pick, or step.
    pub scope: Option<Step<AgentScope>>,
    /// The machine to pick, or step.
    pub host: Option<Step<HostFilter>>,
}

/// Pick a value, or move to the next one in its cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step<T> {
    /// Pick this value.
    Set(T),
    /// The value after the current one.
    Next,
}

impl FilterRequest {
    /// The filter this request makes of `current`, where `hosts` are the
    /// satellite machines that have agents ([`agent_hosts`]).
    #[must_use]
    pub fn apply(&self, current: &AgentFilter, hosts: &[String]) -> AgentFilter {
        let scope = match &self.scope {
            Some(Step::Set(scope)) => *scope,
            Some(Step::Next) => current.scope.next(),
            None if self.host.is_none() => current.scope.next(),
            None => current.scope,
        };
        let host = match &self.host {
            Some(Step::Set(host)) => host.clone(),
            Some(Step::Next) => current.host.next(hosts),
            None => current.host.clone(),
        };
        AgentFilter { scope, host }
    }
}

/// The satellite hosts among `agents`, in first-seen order: the machines
/// the `@` chip can step to besides every machine and here.
#[must_use]
pub fn agent_hosts(agents: &[AgentEntry]) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for host in agents.iter().filter_map(|entry| entry.host.as_ref()) {
        if !hosts.contains(host) {
            hosts.push(host.clone());
        }
    }
    hosts
}

/// One chip of the filter row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterChip {
    /// Picks this scope.
    Scope(AgentScope),
    /// Steps the machine filter.
    Host,
}

/// Narrowest text area the filter row needs: the three scope chips and the
/// single spaces between them.
pub const MIN_FILTER_COLS: u16 = 13;

/// The filter row's chips as `(start, width, chip)` in text-area columns.
///
/// The scope chips sit at fixed offsets, then the `@` chip takes whatever
/// the row has left once `host_chip` asks for it. Each chip's click target
/// is its label; the separating spaces are inert. Empty below
/// [`MIN_FILTER_COLS`].
#[must_use]
pub fn filter_chips(text_w: u16, host_chip: bool) -> Vec<(u16, u16, FilterChip)> {
    if text_w < MIN_FILTER_COLS {
        return Vec::new();
    }
    let mut chips = Vec::with_capacity(4);
    let mut x = 0u16;
    for scope in AgentScope::ALL {
        let w = u16::try_from(scope.label().len()).unwrap_or(u16::MAX);
        chips.push((x, w, FilterChip::Scope(scope)));
        x = x.saturating_add(w + 1);
    }
    // The `@` chip needs room for `@` and at least two more cells.
    if host_chip && text_w >= x + 3 {
        chips.push((x, text_w - x, FilterChip::Host));
    }
    chips
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client::agent_meta::AgentMetaState;

    fn entry(session: Option<&str>, host: Option<&str>, state: AgentMetaState) -> AgentEntry {
        AgentEntry {
            session: session.map(str::to_owned),
            session_id: None,
            resource: None,
            window: 0,
            window_name: "w".to_owned(),
            pane: None,
            name: "claude".to_owned(),
            state,
            attention: false,
            host: host.map(str::to_owned),
            seen: false,
        }
    }

    /// Scope and machine compose: `this` keeps our session's rows, `need`
    /// keeps the attention rungs, and a host filter keeps that machine's.
    #[test]
    fn scope_and_host_compose() {
        let ours = entry(None, None, AgentMetaState::Working);
        let peer_blocked = entry(Some("api"), None, AgentMetaState::Blocked);
        let satellite = entry(Some("edge"), Some("edge"), AgentMetaState::Working);
        let filter = |scope, host| AgentFilter { scope, host };
        let all = filter(AgentScope::All, HostFilter::Any);
        assert!(all.admits(&ours) && all.admits(&peer_blocked) && all.admits(&satellite));
        let this = filter(AgentScope::Session, HostFilter::Any);
        assert!(this.admits(&ours) && !this.admits(&peer_blocked));
        let need = filter(AgentScope::Attention, HostFilter::Any);
        assert!(!need.admits(&ours) && need.admits(&peer_blocked));
        let here = filter(AgentScope::All, HostFilter::Here);
        assert!(here.admits(&ours) && !here.admits(&satellite));
        let edge = filter(AgentScope::All, HostFilter::Host("edge".to_owned()));
        assert!(edge.admits(&satellite) && !edge.admits(&ours));
    }

    /// The `@` chip steps every machine, here, each satellite, and wraps.
    #[test]
    fn host_filter_cycles_through_the_machines_with_agents() {
        let hosts = vec!["edge".to_owned(), "gpu".to_owned()];
        let mut filter = HostFilter::Any;
        let mut seen = Vec::new();
        for _ in 0..5 {
            filter = filter.next(&hosts);
            seen.push(filter.label());
        }
        assert_eq!(seen, ["@here", "@edge", "@gpu", "@all", "@here"]);
        assert_eq!(
            HostFilter::Host("gone".to_owned()).next(&hosts),
            HostFilter::Any
        );
    }

    /// The scope chips sit at fixed offsets; the `@` chip takes the rest of
    /// a row wide enough for it; a narrow row has no chips at all.
    #[test]
    fn chips_lay_out_at_fixed_offsets() {
        let chips = filter_chips(17, true);
        assert_eq!(
            chips,
            vec![
                (0, 3, FilterChip::Scope(AgentScope::All)),
                (4, 4, FilterChip::Scope(AgentScope::Session)),
                (9, 4, FilterChip::Scope(AgentScope::Attention)),
                (14, 3, FilterChip::Host),
            ]
        );
        assert_eq!(filter_chips(17, false).len(), 3);
        assert!(filter_chips(MIN_FILTER_COLS - 1, true).is_empty());
    }

    /// A bare request steps the scope; each half otherwise sets or steps only
    /// its own part.
    #[test]
    fn requests_set_or_step_their_half() {
        let hosts = vec!["edge".to_owned()];
        let start = AgentFilter::default();
        let bare = FilterRequest::default().apply(&start, &hosts);
        assert_eq!(bare.scope, AgentScope::Session);
        assert_eq!(bare.host, HostFilter::Any);
        let host_only = FilterRequest {
            scope: None,
            host: Some(Step::Next),
        }
        .apply(&start, &hosts);
        assert_eq!(host_only.scope, AgentScope::All);
        assert_eq!(host_only.host, HostFilter::Here);
        let pick = FilterRequest {
            scope: Some(Step::Set(AgentScope::Attention)),
            host: Some(Step::Set(HostFilter::Host("edge".to_owned()))),
        }
        .apply(&start, &hosts);
        assert_eq!(pick.scope, AgentScope::Attention);
        assert_eq!(pick.host, HostFilter::Host("edge".to_owned()));
    }

    /// Bindings may name a scope by its argument or its chip label.
    #[test]
    fn scopes_round_trip_through_their_names() {
        for scope in AgentScope::ALL {
            assert_eq!(AgentScope::parse(scope.name()), Some(scope));
            assert_eq!(AgentScope::parse(scope.label()), Some(scope));
        }
        assert_eq!(AgentScope::parse("bogus"), None);
    }
}
