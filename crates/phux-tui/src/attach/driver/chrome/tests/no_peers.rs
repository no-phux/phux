//! The empty peer projection for chrome tests that need no cross-session state.

use super::*;

/// An empty peer bundle for unit tests that exercise the chrome refresh
/// without any cross-session state.
pub(super) fn no_peers() -> crate::attach::sidebar_zones::PeerInputs<'static> {
    use std::sync::LazyLock;
    static SESSIONS: &[phux_protocol::wire::info::SessionInfo] = &[];
    static LAYOUTS: LazyLock<HashMap<phux_protocol::ids::SessionId, ArrangementDoc>> =
        LazyLock::new(HashMap::new);
    static AGENTS: LazyLock<HashMap<ResourceId, AgentRecord>> = LazyLock::new(HashMap::new);
    static ATTENTION: LazyLock<std::collections::HashSet<ResourceId>> =
        LazyLock::new(std::collections::HashSet::new);
    static TAGS: LazyLock<std::collections::HashMap<phux_protocol::ids::SessionId, String>> =
        LazyLock::new(std::collections::HashMap::new);
    static COLLAPSED: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    static WINDOWS: &[phux_protocol::wire::info::WindowInfo] = &[];
    static RESOURCES: &[phux_protocol::wire::info::ResourceInfo] = &[];
    static REVIEW: LazyLock<ReviewIndex> = LazyLock::new(ReviewIndex::new);
    crate::attach::sidebar_zones::PeerInputs {
        serving_host: None,
        origin: None,
        remote_hosts: &[],
        hosts: &[],
        sessions: SESSIONS,
        focused_session: None,
        windows: WINDOWS,
        resources: RESOURCES,
        foreign_layouts: &LAYOUTS,
        foreign_agents: &AGENTS,
        foreign_attention: &ATTENTION,
        project_tags: &TAGS,
        session_order: &[],
        collapsed_projects: &COLLAPSED,
        review: &REVIEW,
    }
}
