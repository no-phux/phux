//! The collapsed rail: badge, session dots, overflow, and the expand row.

use super::*;
use crate::render::chrome::sidebar::rail::{
    ACTIVE_MARK, EXPAND_GLYPH, OVERFLOW_MARK, is_rail, rail_model,
};

fn rail_painter() -> SidebarPainter {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_agents(vec![
        agent(0, "build", "claude", AgentMetaState::Blocked),
        agent(1, "docs", "codex", AgentMetaState::Working),
    ]);
    p.set_roster(vec![
        roster("api", 1, 0, 0),
        active_roster(),
        roster("web", 0, 2, 0),
    ]);
    p
}

/// Only a strip too narrow for row text paints as the rail.
#[test]
fn only_a_glyph_wide_strip_is_the_rail() {
    assert!(is_rail(RAIL_WIDTH));
    assert!(!is_rail(RAIL_WIDTH + 1));
}

/// Top to bottom: the agents badge, a gap, a dot per session, and the
/// expand chevron in the rule's bottom cell. The active session is marked
/// on the outer column.
#[test]
fn the_rail_keeps_the_badge_and_a_dot_per_session() {
    let p = rail_painter();
    let rect = r(0, 0, RAIL_WIDTH, 8);
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert_eq!(buf[(1, 0)].symbol(), "1", "one agent needs you");
    assert_eq!(
        buf[(0, 0)].symbol(),
        "\u{25cf}",
        "counted as the status bar does"
    );
    for y in 2..5 {
        assert_ne!(buf[(1, y)].symbol(), " ", "session dot on row {y}");
    }
    assert_eq!(buf[(0, 3)].symbol(), ACTIVE_MARK, "the active session");
    assert_eq!(buf[(0, 2)].symbol(), " ");
    assert_eq!(buf[(1, 2)].fg, Theme::default().agent_blocked);
    assert_eq!(buf[(2, 0)].symbol(), "│");
    assert_eq!(buf[(2, 7)].symbol(), EXPAND_GLYPH);

    let mirrored = p.compose_buffer(rect, SidebarRule::Leading, &[]);
    assert_eq!(mirrored[(0, 0)].symbol(), "│");
    assert_eq!(mirrored[(1, 0)].symbol(), "1");
    assert_eq!(mirrored[(2, 3)].symbol(), ACTIVE_MARK);
    assert_eq!(mirrored[(0, 7)].symbol(), COLLAPSE_GLYPH);
}

/// Clicks resolve through the strip's own targets: the badge opens the
/// fleet, a dot switches to its session, any cell of the last row expands,
/// and the rule is part of each row's target.
#[test]
fn rail_clicks_reuse_the_strip_targets() {
    let p = rail_painter();
    let rect = r(0, 0, RAIL_WIDTH, 8);
    let c = p.counts();
    assert_eq!(hit_test(rect, c, 1, 0), Some(SidebarHit::Fleet));
    assert_eq!(hit_test(rect, c, 1, 1), None);
    assert_eq!(hit_test(rect, c, 0, 4), Some(SidebarHit::Roster(2)));
    assert_eq!(hit_test(rect, c, 2, 3), Some(SidebarHit::Roster(1)));
    for x in 0..RAIL_WIDTH {
        assert_eq!(hit_test(rect, c, x, 7), Some(SidebarHit::Collapse));
    }
    assert_eq!(hit_test(rect, c, 1, 6), None);
}

/// Cut short, the dots end at the active session and the next row marks
/// the rest; `agents-share = 0` drops the badge.
#[test]
fn a_short_rail_keeps_the_active_session() {
    let c = SidebarCounts {
        active_session: Some(5),
        ..counts(0, 0, 8)
    };
    assert_eq!(
        rail_model(c, 6),
        vec![
            SidebarRow::NeedsYouHeader,
            SidebarRow::Blank,
            SidebarRow::RosterEntry(4),
            SidebarRow::RosterEntry(5),
            SidebarRow::RosterOverflow,
            SidebarRow::Blank,
        ]
    );
    let no_agents = SidebarCounts {
        agents_share: 0,
        ..c
    };
    assert_eq!(
        rail_model(no_agents, 4),
        vec![
            SidebarRow::RosterEntry(4),
            SidebarRow::RosterEntry(5),
            SidebarRow::RosterOverflow,
            SidebarRow::Blank,
        ]
    );

    let mut p = SidebarPainter::new(Theme::default());
    p.set_roster((0..8).map(|i| roster(&format!("s{i}"), 0, 0, 0)).collect());
    let buf = p.compose_buffer(r(0, 0, RAIL_WIDTH, 6), SidebarRule::Trailing, &[]);
    assert_eq!(buf[(1, 0)].symbol(), "0", "nothing needs you");
    assert_eq!(buf[(0, 0)].symbol(), " ", "and no needs-you mark");
    assert_eq!(buf[(1, 4)].symbol(), OVERFLOW_MARK);
}
