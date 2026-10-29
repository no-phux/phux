//! The hit test, and its agreement with the painter.

use super::*;

#[test]
fn hit_test_maps_rows_to_targets() {
    let rect = r(0, 0, 20, 14);
    let c = SidebarCounts {
        active_session: Some(0),
        rule: SidebarRule::Trailing,
        agents_share: 0,
        ..counts(0, 2, 1)
    };
    assert_eq!(hit_test(rect, c, 3, 0), Some(SidebarHit::Sessions));
    assert_eq!(hit_test(rect, c, 3, 3), Some(SidebarHit::Window(0)));
    assert_eq!(hit_test(rect, c, 3, 4), Some(SidebarHit::Window(1)));
    // Padding rows miss.
    assert_eq!(hit_test(rect, c, 3, 5), None);
    // Footer row.
    assert_eq!(hit_test(rect, c, 3, 13), Some(SidebarHit::NewWindow));
}

/// Agent and session rows retain their own destinations under overflow.
#[test]
fn hit_test_maps_the_new_zones() {
    let rect = r(0, 0, 20, 10);
    // 2 agents + 1 tab: rows 0 header, 1 filter chips, 2-3 agents, then
    // blank.
    let c = counts(2, 1, 0);
    assert_eq!(hit_test(rect, c, 3, 0), Some(SidebarHit::Fleet));
    assert_eq!(
        hit_test(rect, c, 3, 1),
        Some(SidebarHit::AgentFilter(FilterChip::Scope(AgentScope::All)))
    );
    // Each chip is its own target; the space between chips is inert.
    assert_eq!(
        hit_test(rect, c, 6, 1),
        Some(SidebarHit::AgentFilter(FilterChip::Scope(
            AgentScope::Session
        )))
    );
    assert_eq!(
        hit_test(rect, c, 11, 1),
        Some(SidebarHit::AgentFilter(FilterChip::Scope(
            AgentScope::Attention
        )))
    );
    assert_eq!(hit_test(rect, c, 4, 1), None, "between chips");
    assert_eq!(hit_test(rect, c, 3, 2), Some(SidebarHit::NeedsYou(0)));
    assert_eq!(hit_test(rect, c, 3, 3), Some(SidebarHit::NeedsYou(1)));
    assert_eq!(hit_test(rect, c, 3, 4), None, "blank row");
    // The `@` chip only when the agents span machines.
    assert_eq!(hit_test(rect, c, 16, 1), None);
    let hosts = SidebarCounts {
        agent_host_chip: true,
        ..c
    };
    assert_eq!(
        hit_test(rect, hosts, 16, 1),
        Some(SidebarHit::AgentFilter(FilterChip::Host))
    );

    // Roster rows, below the fixed Agents panel (header + quiet
    // placeholder + padding in a 12-row strip).
    let tall = Rect { h: 12, ..rect };
    let c = counts(0, 1, 2);
    assert_eq!(hit_test(tall, c, 3, 0), Some(SidebarHit::Fleet));
    assert_eq!(hit_test(tall, c, 3, 2), None, "the quiet placeholder");
    let sessions = agents_budget(c.agents_share, usize::from(tall.h) - 1);
    assert_eq!(
        hit_test(tall, c, 3, u16::try_from(sessions).unwrap()),
        Some(SidebarHit::Sessions)
    );
    assert_eq!(
        hit_test(tall, c, 3, u16::try_from(sessions + 1).unwrap()),
        Some(SidebarHit::Roster(0))
    );
    assert_eq!(
        hit_test(tall, c, 3, u16::try_from(sessions + 2).unwrap()),
        Some(SidebarHit::Roster(0))
    );
    assert_eq!(
        hit_test(tall, c, 3, u16::try_from(sessions + 3).unwrap()),
        Some(SidebarHit::Roster(1))
    );
    assert_eq!(
        hit_test(tall, c, 3, u16::try_from(sessions + 4).unwrap()),
        Some(SidebarHit::Roster(1))
    );

    // Overflow rows open the dashboard — the surface that has what the
    // strip had to drop.
    let big = Rect { h: 16, ..rect };
    let c = counts(9, 1, 0);
    let model = row_model(c, big.h);
    let row = model
        .iter()
        .position(|r| matches!(r, SidebarRow::NeedsYouOverflow))
        .expect("overflow allocated");
    assert_eq!(
        hit_test(big, c, 3, u16::try_from(row).unwrap()),
        Some(SidebarHit::Fleet)
    );
    let c = counts(0, 0, 9);
    let model = row_model(c, tall.h);
    let row = model
        .iter()
        .position(|r| *r == SidebarRow::RosterOverflow)
        .expect("overflow");
    assert_eq!(
        hit_test(tall, c, 3, u16::try_from(row).unwrap()),
        Some(SidebarHit::Sessions)
    );
}

/// The bottom corner cell is the collapse chevron — the
/// only interactive cell on the separator column.
#[test]
fn hit_test_resolves_the_collapse_corner() {
    let rect = r(0, 0, 20, 8);
    let c = counts(0, 1, 0);
    assert_eq!(hit_test(rect, c, 19, 7), Some(SidebarHit::Collapse));
    // The rest of the separator column stays inert.
    assert_eq!(hit_test(rect, c, 19, 6), None);
    assert_eq!(hit_test(rect, c, 19, 0), None);
    // No footer (short strip) => no chevron target.
    let short = r(0, 0, 20, 3);
    assert_eq!(hit_test(short, c, 19, 2), None);
}

#[test]
fn hit_test_respects_the_rect_origin_and_separator() {
    // A strip at x=60, y=2; agent 0 is at local row 2, under the
    // header and the filter chips.
    let rect = r(60, 2, 20, 8);
    let c = counts(1, 0, 0);
    assert_eq!(hit_test(rect, c, 60, 4), Some(SidebarHit::NeedsYou(0)));
    assert_eq!(
        hit_test(rect, c, 62, 3),
        Some(SidebarHit::AgentFilter(FilterChip::Scope(AgentScope::All)))
    );
    // The separator column (last column of the strip) is not a target
    // outside the chevron corner.
    assert_eq!(hit_test(rect, c, 79, 0), None);
    // Outside the strip entirely.
    assert_eq!(hit_test(rect, c, 59, 1), None);
    assert_eq!(hit_test(rect, c, 80, 1), None);
    assert_eq!(hit_test(rect, c, 60, 10), None);
    // Degenerate rects never hit.
    assert_eq!(hit_test(r(0, 0, 0, 0), c, 0, 0), None);
}

/// The filter row paints its chips, and its first cell under the probe
/// column is the `all` chip.
fn assert_filter_row(text: &str, hit: Option<SidebarHit>) {
    assert!(text.contains(AgentScope::All.label()), "{text:?}");
    assert_eq!(
        hit,
        Some(SidebarHit::AgentFilter(FilterChip::Scope(AgentScope::All)))
    );
}

/// Paint and hit-test derive from one row model: every row the painter
/// fills with a window label hit-tests to that window, agent rows
/// hit-test to their windows, and the footer row hit-tests to its
/// affordance.
#[test]
fn paint_and_hit_test_agree_row_for_row() {
    let rect = r(0, 0, 26, 22);
    let windows = vec![
        win_branch("alpha", true, "main"),
        win("beta", false),
        win_branch("gamma", false, "dev"),
    ];
    let agents = vec![
        agent(1, "beta", "claude", AgentMetaState::Blocked),
        agent(2, "gamma", "codex", AgentMetaState::Done),
    ];
    let peers = vec![roster("delta", 1, 0, 0), roster("epsilon", 0, 1, 0)];
    let sections = vec![plugin_section("Builds", 2, &["lint", "test", "docs"])];
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(windows.clone());
    p.set_agents(agents.clone());
    p.set_roster(peers.clone());
    p.set_plugin_sections(sections.clone());
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    let c = p.counts();
    for (y, row) in row_model(c, rect.h).iter().enumerate() {
        let y16 = u16::try_from(y).expect("row fits u16");
        let hit = hit_test(rect, c, 2, y16);
        // Exhaustive on purpose: a new SidebarRow variant must fail to
        // compile here rather than slip through a catch-all with no
        // paint/click agreement check of its own.
        match row {
            SidebarRow::NeedsYouHeader => {
                assert!(row_text(&buf, rect, y16).contains(NEEDS_YOU_HEADER));
                assert_eq!(hit, Some(SidebarHit::Fleet));
            }
            SidebarRow::AgentFilter => assert_filter_row(&row_text(&buf, rect, y16), hit),
            SidebarRow::SpacesHeader => {
                assert!(row_text(&buf, rect, y16).contains(SPACES_HEADER));
                assert_eq!(hit, Some(SidebarHit::Sessions));
            }
            SidebarRow::WindowName(i) => {
                assert!(row_text(&buf, rect, y16).contains(&windows[*i].name));
                assert_eq!(hit, Some(SidebarHit::Window(*i)));
            }
            SidebarRow::NeedsYou(j) => {
                assert!(
                    [&agents[*j].window_name, &agents[*j].name]
                        .iter()
                        .all(|label| row_text(&buf, rect, y16).contains(label.as_str()))
                );
                assert_eq!(hit, Some(SidebarHit::NeedsYou(*j)));
            }
            SidebarRow::RosterEntry(j) => {
                assert!(row_text(&buf, rect, y16).contains(&peers[*j].name));
                assert_eq!(hit, Some(SidebarHit::Roster(*j)));
            }
            SidebarRow::RosterHost(j) => {
                assert!(row_text(&buf, rect, y16).contains(&peers[*j].host));
                assert_eq!(hit, Some(SidebarHit::Roster(*j)));
            }
            SidebarRow::NeedsYouOverflow => {
                assert!(row_text(&buf, rect, y16).contains("more"));
                assert_eq!(hit, Some(SidebarHit::Fleet));
            }
            SidebarRow::RosterOverflow => {
                assert_eq!(hit, Some(SidebarHit::Sessions));
            }
            SidebarRow::Blank | SidebarRow::NeedsYouEmpty => {
                assert_eq!(hit, None);
            }
            SidebarRow::NewWindow => {
                assert!(row_text(&buf, rect, y16).contains(NEW_LABEL));
                assert_eq!(hit, Some(SidebarHit::NewWindow));
            }
            SidebarRow::PluginHeader(_)
            | SidebarRow::PluginEntry(..)
            | SidebarRow::PluginOverflow(_)
            | SidebarRow::PluginEmpty(_) => {
                assert_plugin_row(*row, &row_text(&buf, rect, y16), hit, &sections);
            }
        }
    }
    assert!(
        row_model(c, rect.h).contains(&SidebarRow::PluginOverflow(0)),
        "the fixture overflows its section"
    );
}

/// The plugin half of [`paint_and_hit_test_agree_row_for_row`].
fn assert_plugin_row(
    row: SidebarRow,
    text: &str,
    hit: Option<SidebarHit>,
    sections: &[PluginSection],
) {
    match row {
        SidebarRow::PluginHeader(s) => {
            assert!(text.contains(&sections[s].title));
            assert_eq!(hit, None);
        }
        SidebarRow::PluginEntry(s, j) => {
            assert!(text.contains(&sections[s].entries[j].text));
            assert_eq!(hit, Some(SidebarHit::Plugin(s, j)));
        }
        SidebarRow::PluginOverflow(_) => {
            assert!(text.contains("+2"));
            assert_eq!(hit, None);
        }
        _ => assert_eq!(hit, None),
    }
}
