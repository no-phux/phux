//! The Agents panel and the session roster.

use super::*;

/// Every lifecycle stays in the caller's supplied display order.
#[test]
fn agents_section_renders_state_and_name_rows() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win("phux", true), win("scratch", false)]);
    p.set_roster(vec![active_roster()]);
    p.set_agents(vec![
        agent(0, "phux", "claude", AgentMetaState::Blocked),
        agent(1, "scratch", "merge-queue-w5", AgentMetaState::Done),
    ]);
    let rect = r(0, 0, 36, 14);
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert!(
        row_text(&buf, rect, 0).contains(NEEDS_YOU_HEADER),
        "the queue tops the strip: {:?}",
        row_text(&buf, rect, 0)
    );
    assert!(
        row_text(&buf, rect, 1).contains(AgentScope::All.label()),
        "the filter chips sit under the header"
    );
    let claude_row = row_text(&buf, rect, 2);
    assert!(
        claude_row.contains("phux") && claude_row.contains("claude"),
        "queue row shows locator + state - name: {claude_row:?}"
    );
    let worker_row = row_text(&buf, rect, 3);
    assert!(
        worker_row.contains("scratch") && worker_row.contains("merge-queue-w5"),
        "second queue row: {worker_row:?}"
    );
    assert!(
        row_text(&buf, rect, 7).contains(SPACES_HEADER),
        "Sessions lands on the fixed line below the Agents panel: {:?}",
        row_text(&buf, rect, 7)
    );
    p.set_agents(vec![
        agent(0, "phux", "claude", AgentMetaState::Done),
        agent(1, "scratch", "merge-queue-w5", AgentMetaState::Blocked),
    ]);
    let changed = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert!(row_text(&changed, rect, 2).contains("claude"));
    assert!(row_text(&changed, rect, 3).contains("merge-queue-w5"));
    assert_eq!(row_text(&buf, rect, 7), row_text(&changed, rect, 7));
}

/// A cross-session queue row is labelled by its SESSION, not
/// by its window — "edit" locates nothing once the row can come from
/// anywhere on the server.
#[test]
fn a_foreign_queue_row_is_labelled_by_its_session() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win("phux", true)]);
    p.set_agents(vec![AgentEntry {
        session: Some("phux-feat-auth".to_owned()),
        pane: Some(1),
        ..agent(0, "edit", "claude", AgentMetaState::Blocked)
    }]);
    let rect = r(0, 0, 36, 14);
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    let row = row_text(&buf, rect, 2);
    assert!(
        row.contains("phux-feat-auth"),
        "foreign row names its session: {row:?}"
    );
    assert!(
        !row.contains("edit"),
        "window name is not the locator: {row:?}"
    );
}

/// ADR-0136: a satellite agent row badges its host and opens that pane.
#[test]
fn a_satellite_agent_row_badges_its_host_and_opens_the_pane() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win("phux", true)]);
    let resource = ResourceId::satellite("gpubox", 4);
    p.set_agents(vec![AgentEntry {
        session: Some("reviewer".to_owned()),
        resource: Some(resource.clone()),
        host: Some("gpubox".to_owned()),
        ..agent(0, "edit", "reviewer", AgentMetaState::Blocked)
    }]);
    let rect = r(0, 0, 36, 14);
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    // Agents on a satellite add the `@` machine chip to the filter row.
    assert!(row_text(&buf, rect, 1).contains("@all"));
    let row = row_text(&buf, rect, 2);
    assert!(row.contains("gpubox"), "host badge: {row:?}");
    assert!(row.contains("reviewer"), "agent name: {row:?}");
    let action = satellite_open_action(&resource).expect("satellite click opens");
    assert_eq!(action.action, "split-pane");
    assert_eq!(
        action.args.get("resource").and_then(|value| value.as_str()),
        Some("gpubox/@4")
    );
    assert!(satellite_open_action(&ResourceId::local(1)).is_none());
}

/// A queue click must carry the peer's `SessionId` so a
/// rename of that session cannot retarget the switch.
#[test]
fn a_foreign_queue_click_carries_session_identity() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_agents(vec![AgentEntry {
        session: Some("stale".to_owned()),
        session_id: Some(SessionId::new(7)),
        pane: Some(1),
        ..agent(0, "edit", "claude", AgentMetaState::Blocked)
    }]);
    assert_eq!(
        p.click_targets().needs_you,
        vec![SidebarTarget::Session {
            name: "stale".to_owned(),
            id: Some(SessionId::new(7)),
            window: Some(0),
            pane: Some(1),
            resource: None,
        }]
    );
    // A graph-discovered agent carries its `ResourceId` and no invented
    // window/pane indices.
    p.set_agents(vec![AgentEntry {
        session: Some("peer".to_owned()),
        session_id: Some(SessionId::new(2)),
        resource: Some(ResourceId::local(10)),
        pane: None,
        ..agent(0, "main", "reviewer", AgentMetaState::Blocked)
    }]);
    assert_eq!(
        p.click_targets().needs_you,
        vec![SidebarTarget::Session {
            name: "peer".to_owned(),
            id: Some(SessionId::new(2)),
            window: None,
            pane: None,
            resource: Some(ResourceId::local(10)),
        }]
    );
}

/// ADR-0112: a calm fleet keeps the Agents panel (header and quiet
/// placeholder) so Sessions never moves.
#[test]
fn a_quiet_fleet_keeps_the_agents_panel() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win_branch("phux", true, "main")]);
    p.set_roster(vec![active_roster()]);
    p.set_agents(vec![agent(0, "phux", "claude", AgentMetaState::Working)]);
    p.set_agent_filter(AgentFilter {
        scope: AgentScope::Attention,
        host: HostFilter::Any,
    });
    let rect = r(0, 0, 24, 12);
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert!(
        row_text(&buf, rect, 0).contains(NEEDS_YOU_HEADER),
        "the Agents panel keeps its header when calm: {:?}",
        row_text(&buf, rect, 0)
    );
    assert!(
        row_text(&buf, rect, 2).contains(AGENTS_EMPTY_LABEL),
        "a quiet panel shows its placeholder: {:?}",
        row_text(&buf, rect, 2)
    );
    // Under any other filter an empty panel says nothing is running.
    p.set_agents(Vec::new());
    p.set_agent_filter(AgentFilter::default());
    let empty = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert!(row_text(&empty, rect, 2).contains(AGENTS_NONE_LABEL));
    let sessions = row_model(p.counts(), rect.h)
        .iter()
        .position(|r| *r == SidebarRow::SpacesHeader)
        .expect("sessions laid");
    assert_eq!(
        sessions,
        agents_budget(DEFAULT_AGENTS_SHARE, usize::from(rect.h) - 1),
        "Sessions sits on the fixed line whatever the population"
    );
    assert_eq!(p.counts().needs_you, 0);
}

/// `agents-share` is a live input: `0` drops the band, a larger share
/// pushes Sessions down, and changing it dirties the strip so a caller
/// gating on the report repaints.
#[test]
fn agents_share_knob_controls_the_band() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win("phux", true)]);
    p.set_roster(vec![active_roster()]);
    p.set_agents(vec![agent(0, "phux", "claude", AgentMetaState::Blocked)]);
    let rect = r(0, 0, 24, 12);
    let sessions_at = |p: &SidebarPainter| {
        row_model(p.counts(), rect.h)
            .iter()
            .position(|r| *r == SidebarRow::SpacesHeader)
            .expect("sessions laid")
    };
    assert_eq!(row_model(p.counts(), rect.h)[0], SidebarRow::NeedsYouHeader);
    let banded = sessions_at(&p);
    assert!(p.set_agents_share(0), "a change reports");
    assert!(!p.set_agents_share(0), "the same share is a no-op");
    assert_eq!(
        row_model(p.counts(), rect.h)[0],
        SidebarRow::SpacesHeader,
        "0 drops the Agents band"
    );
    assert!(p.set_agents_share(80));
    assert!(
        sessions_at(&p) > banded,
        "a larger share pushes Sessions down"
    );
}

/// With nothing to list, the strip paints no section headers at all.
#[test]
fn an_empty_strip_paints_no_headers() {
    let p = SidebarPainter::new(Theme::default());
    let rect = r(0, 0, 24, 12);
    let text = strip_text(&p, rect);
    assert!(!text.contains(SPACES_HEADER), "{text}");
    assert!(!text.contains(NEEDS_YOU_HEADER), "{text}");
    assert_eq!(hit_test(rect, p.counts(), 3, 0), None);
}

/// Each session has a histogram name row and explicit host identity.
#[test]
fn the_roster_renders_host_pairs_with_counts() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win("phux", true)]);
    p.set_roster(vec![
        roster("feat-auth", 1, 2, 0),
        SessionRosterEntry {
            unknown: 4,
            satellite: true,
            host: "devbox".to_owned(),
            ..roster("prod-3", 0, 0, 0)
        },
    ]);
    let rect = r(0, 0, 26, 16);
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    let model = row_model(p.counts(), rect.h);
    let first = model
        .iter()
        .position(|r| matches!(r, SidebarRow::RosterEntry(0)))
        .expect("roster row 0 allocated");
    let busy = row_text(&buf, rect, u16::try_from(first).unwrap());
    assert!(busy.contains("feat-auth"), "session name: {busy:?}");
    assert!(
        busy.contains("●1") && !busy.contains('◐'),
        "histogram counts only what needs a human: {busy:?}"
    );
    // Each machine's header sits directly above its sessions.
    assert!(row_text(&buf, rect, u16::try_from(first - 1).unwrap()).contains("mini"));
    assert!(row_text(&buf, rect, u16::try_from(first + 1).unwrap()).contains("devbox"));
    let sat = row_text(&buf, rect, u16::try_from(first + 2).unwrap());
    assert!(sat.contains("prod-3"), "satellite name: {sat:?}");
    assert!(!sat.contains('?'), "no unknown-count noise: {sat:?}");
}
