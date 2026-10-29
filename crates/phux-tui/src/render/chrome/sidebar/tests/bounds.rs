//! Click targets, stable-row repaints, overflow, and size bounds.

use super::*;

#[test]
fn roster_targets_preserve_host_routes_and_disable_placeholders() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_roster(vec![
        active_roster(),
        SessionRosterEntry {
            host: "devbox".to_owned(),
            route_host: Some("satellite-dev".to_owned()),
            satellite: true,
            ..roster("development", 0, 0, 0)
        },
        SessionRosterEntry {
            name: "unreachable".to_owned(),
            host: "offline-host".to_owned(),
            ..SessionRosterEntry::default()
        },
    ]);
    let targets = p.click_targets();
    assert_eq!(targets.counts.active_session, Some(0));
    assert_eq!(
        targets.roster,
        vec![
            Some(SessionRosterTarget {
                name: "development".to_owned(),
                id: None,
                host: None,
                switch_host: None,
            }),
            Some(SessionRosterTarget {
                name: "development".to_owned(),
                id: None,
                host: Some("satellite-dev".to_owned()),
                switch_host: None,
            }),
            None,
        ]
    );
    let rect = r(2, 3, 32, 18);
    let model = row_model(targets.counts, rect.h);
    let y = |row: SidebarRow| {
        rect.y + u16::try_from(model.iter().position(|r| *r == row).expect("row")).expect("y")
    };
    for (row, hit) in [
        (SidebarRow::RosterHost(0), SidebarHit::Roster(0)),
        (SidebarRow::RosterEntry(0), SidebarHit::Roster(0)),
        (SidebarRow::RosterHost(1), SidebarHit::Roster(1)),
        (SidebarRow::RosterEntry(1), SidebarHit::Roster(1)),
    ] {
        assert_eq!(hit_test(rect, targets.counts, 4, y(row)), Some(hit));
    }
}

#[test]
fn changing_agent_state_repaints_only_that_stable_row() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_roster(vec![active_roster()]);
    let first = agent(0, "editor", "claude", AgentMetaState::Blocked);
    let second = agent(1, "runner", "codex", AgentMetaState::Done);
    p.set_agents(vec![first.clone(), second.clone()]);
    let rect = r(0, 0, 36, 18);
    paint_to_string(&mut p, rect);
    p.set_agents(vec![
        first,
        AgentEntry {
            state: AgentMetaState::Blocked,
            ..second
        },
    ]);
    let changed = paint_to_string(&mut p, rect);
    assert_eq!(rows_of(&changed).len(), 1);
    // Header, filter chips, then the agents: the second agent is row 4.
    assert!(changed.starts_with("\x1b[4;1H"));
    assert!(strip_ansi(&changed).contains("codex"));
    assert!(paint_to_string(&mut p, rect).is_empty());
}

#[test]
fn overflow_counts_sessions_and_windows_without_counting_host_lines() {
    let c = SidebarCounts {
        active_session: Some(0),
        rule: SidebarRule::Trailing,
        ..counts(9, 5, 4)
    };
    let model = row_model(c, 14);
    let hidden = hidden_counts(c, &model);
    // The filter chip row takes one of the panel's rows.
    assert_eq!(hidden.needs_you, 5);
    assert_eq!(hidden.roster, 3);
    assert_eq!(hidden.windows, 3);
    assert!(model.contains(&SidebarRow::RosterOverflow));
    assert!(model.contains(&SidebarRow::RosterHost(0)));
    let p = SidebarPainter::new(Theme::default());
    let line = p.sessions_overflow_line(hidden, 32);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "+3 sessions, +3 tabs");
}

#[test]
fn tiny_viewports_and_huge_counts_remain_bounded() {
    let c = SidebarCounts {
        needs_you: usize::MAX,
        windows: usize::MAX,
        roster: usize::MAX,
        active_session: Some(0),
        host_starts: u128::MAX,
        project_starts: 0,
        rule: SidebarRule::Trailing,
        plugin: PluginShape::default(),
        agents_share: DEFAULT_AGENTS_SHARE,
        agent_host_chip: false,
    };
    assert_eq!(row_model(c, 1), vec![SidebarRow::RosterOverflow]);
    let rect = r(0, 0, 20, 1);
    assert_eq!(hit_test(rect, c, 2, 0), Some(SidebarHit::Sessions));
    for h in 0..30 {
        let model = row_model(c, h);
        assert_eq!(model.len(), usize::from(h));
        assert_model_items(c, &model);
    }
    let invalid = SidebarCounts {
        active_session: Some(9),
        rule: SidebarRule::Trailing,
        ..counts(0, 100, 1)
    };
    assert!(
        !row_model(invalid, 18)
            .iter()
            .any(|r| matches!(r, SidebarRow::WindowName(_)))
    );
}

#[test]
fn short_widths_clip_unicode_hosts_and_overflow_inside_separator() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win("编辑器", true)]);
    p.set_roster(vec![
        SessionRosterEntry {
            host: "日本語-devbox".to_owned(),
            ..active_roster()
        },
        roster("peer", 1, 2, 0),
    ]);
    p.set_agents(vec![
        agent(0, "编辑器", "claude", AgentMetaState::Blocked);
        12
    ]);
    for w in 0..20 {
        assert_narrow_paint(&mut p, w);
    }
}

fn assert_narrow_paint(p: &mut SidebarPainter, w: u16) {
    let rect = Rect {
        x: 0,
        y: 0,
        w,
        h: 14,
    };
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    let painted = paint_to_string(p, rect);
    for row in rows_of(&painted) {
        assert_eq!(display_width(&row), usize::from(w), "w={w}: {row:?}");
    }
    if w > 0 {
        assert_eq!(buf[(w - 1, 0)].symbol(), "│");
    }
}
