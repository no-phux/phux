//! `next-attention` (C-a q) across sessions.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;
use phux_protocol::wire::info::SessionInfo;

use super::effects::ReattachTarget;

fn sinfo(id: u32, name: &str) -> SessionInfo {
    SessionInfo::new(SessionId::new(id), name)
}

/// `C-a q` with nothing waiting here goes to the first waiting pane in
/// another session (server order), not the bell the status bar's `●1`
/// contradicted; the current session's own entries never count.
#[test]
fn next_attention_falls_back_to_another_sessions_waiting_pane() {
    use phux_protocol::wire::info::ResourceInfo;
    let sessions = vec![sinfo(1, "work"), sinfo(2, "api"), sinfo(3, "docs")];
    let terminal = |id, session| {
        ResourceInfo::new(
            ResourceId::local(id),
            phux_protocol::ids::WindowId::new(1),
            80,
            24,
        )
        .with_session(Some(SessionId::new(session)))
    };
    let resources = vec![
        terminal(1, 1),
        terminal(2, 2),
        terminal(3, 3),
        terminal(4, 3),
    ];
    let foreign_attention = std::collections::HashSet::from([ResourceId::local(4)]);
    let (layouts, agents, tags) = (HashMap::new(), HashMap::new(), HashMap::new());
    let collapsed = std::collections::BTreeSet::new();
    let review = crate::attach::review::ReviewIndex::default();
    let peers = |focused: u32, attention| crate::attach::sidebar_zones::PeerInputs {
        serving_host: None,
        origin: None,
        remote_hosts: &[],
        hosts: &[],
        sessions: &sessions,
        focused_session: Some(SessionId::new(focused)),
        windows: &[],
        resources: &resources,
        foreign_layouts: &layouts,
        foreign_agents: &agents,
        foreign_attention: attention,
        project_tags: &tags,
        session_order: &[],
        collapsed_projects: &collapsed,
        review: &review,
    };
    let target = super::run_action::waiting_elsewhere(&peers(1, &foreign_attention));
    assert!(
        matches!(
            target,
            Some(ReattachTarget::Existing { ref name, resource: Some(ref id), .. })
                if name == "docs" && *id == ResourceId::local(4)
        ),
        "{target:?}"
    );
    assert!(
        super::run_action::waiting_elsewhere(&peers(3, &foreign_attention)).is_none(),
        "the session being viewed is not elsewhere"
    );
}
