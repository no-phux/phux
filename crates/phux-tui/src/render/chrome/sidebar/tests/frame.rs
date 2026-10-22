//! The strip's frame: footer, rule side, junctions, plugin bands, right edge.

use super::*;

/// The New window footer renders on the strip's last row
/// when the strip is tall enough, and drops out below the minimum.
/// The collapse chevron claims the bottom corner cell.
#[test]
fn footer_affordance_renders_on_the_last_row() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win("shell", true)]);
    let rect = r(0, 0, 28, 8);
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert!(
        row_text(&buf, rect, 7).contains(NEW_LABEL),
        "row 7 should hold the new affordance: {:?}",
        row_text(&buf, rect, 7)
    );
    // The bottom corner cell carries the collapse chevron instead of
    // the separator rule.
    assert_eq!(buf[(27, 7)].symbol(), COLLAPSE_GLYPH);
    assert_eq!(buf[(27, 6)].symbol(), "│");
    // A 3-row strip is below the footer minimum: no affordances, no
    // chevron.
    let short = r(0, 0, 20, 3);
    let plain = strip_ansi(&paint_to_string(&mut p, short));
    assert!(
        !plain.contains(NEW_LABEL),
        "short strip must not render the footer: {plain:?}"
    );
    assert!(
        !plain.contains(COLLAPSE_GLYPH),
        "short strip must not render the chevron: {plain:?}"
    );
}

/// A right-docked strip puts its rule (and the collapse
/// chevron) on its pane-facing first column, and every row target moves
/// one column away from it, out to the screen edge.
#[test]
fn a_leading_rule_mirrors_the_strip_toward_the_panes() {
    let rect = r(52, 0, 28, 14);
    let leading = SidebarCounts {
        active_session: Some(0),
        rule: SidebarRule::Leading,
        agents_share: 0,
        ..counts(0, 1, 1)
    };
    assert_eq!(
        hit_test(rect, leading, 52, 3),
        None,
        "the rule is no row target"
    );
    assert_eq!(hit_test(rect, leading, 52, 13), Some(SidebarHit::Collapse));
    assert_eq!(
        hit_test(rect, leading, 79, 3),
        Some(SidebarHit::Window(0)),
        "the screen-edge column is row content, not a rule"
    );
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win("build", true)]);
    let local = Rect { x: 0, ..rect };
    let buf = p.compose_buffer(local, SidebarRule::Leading, &[]);
    assert_eq!(
        buf.cell((0, 0)).map(ratatui::buffer::Cell::symbol),
        Some("│")
    );
    assert_ne!(
        buf.cell((27, 0)).map(ratatui::buffer::Cell::symbol),
        Some("│")
    );
    assert_eq!(
        buf.cell((0, 13)).map(ratatui::buffer::Cell::symbol),
        Some(COLLAPSE_GLYPH)
    );
}

/// ADR-0148: a plugin section is a fixed band between Agents and
/// Sessions; its population never moves the Sessions header, an empty
/// section shows the quiet dash, and a strip too short for it keeps the
/// two core panels instead.
#[test]
fn plugin_sections_hold_a_fixed_band_between_agents_and_sessions() {
    let rect = r(0, 0, 28, 24);
    let mut p = SidebarPainter::new(Theme::default());
    p.set_roster(vec![active_roster()]);
    p.set_plugin_sections(vec![plugin_section("Builds", 3, &[])]);
    let model = row_model(p.counts(), rect.h);
    let sessions_at = |m: &[SidebarRow]| m.iter().position(|r| *r == SidebarRow::SpacesHeader);
    let empty_sessions = sessions_at(&model);
    let header = model
        .iter()
        .position(|r| *r == SidebarRow::PluginHeader(0))
        .expect("section laid");
    assert!(Some(header) < empty_sessions);
    assert_eq!(model[header + 1], SidebarRow::PluginEmpty(0));
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert!(row_text(&buf, rect, u16::try_from(header).expect("row")).contains("Builds"));

    p.set_plugin_sections(vec![plugin_section(
        "Builds",
        3,
        &["a", "b", "c", "d", "e"],
    )]);
    let full = row_model(p.counts(), rect.h);
    assert_eq!(
        sessions_at(&full),
        empty_sessions,
        "population never moves Sessions"
    );
    assert_eq!(
        &full[header..header + 4],
        &[
            SidebarRow::PluginHeader(0),
            SidebarRow::PluginEntry(0, 0),
            SidebarRow::PluginEntry(0, 1),
            SidebarRow::PluginOverflow(0),
        ]
    );
    let targets = p.click_targets();
    assert_eq!(targets.plugin[0][1], (1, 0));
    assert_eq!(
        hit_test(
            rect,
            targets.counts,
            2,
            u16::try_from(header + 2).expect("row")
        ),
        Some(SidebarHit::Plugin(0, 1))
    );

    // Too short for the band plus the core minimum: no section at all.
    let short = row_model(p.counts(), 12);
    assert!(
        !short
            .iter()
            .any(|r| matches!(r, SidebarRow::PluginHeader(_)))
    );
    assert!(short.contains(&SidebarRow::SpacesHeader));
}

/// The rail and any divider reaching the strip tee into the separator on
/// the pane-facing side, so the grid and the strip read as one frame.
#[test]
fn the_rail_junction_tees_into_the_panes() {
    let p = SidebarPainter::new(Theme::default());
    let rect = r(0, 0, 20, 10);
    let left = p.compose_buffer(rect, SidebarRule::Trailing, &[1]);
    assert_eq!(left[(19, 1)].symbol(), "├");
    assert_eq!(left[(19, 2)].symbol(), "│");
    let right = p.compose_buffer(rect, SidebarRule::Leading, &[1]);
    assert_eq!(right[(0, 1)].symbol(), "┤");
    let none = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert_eq!(none[(19, 1)].symbol(), "│");
    // A horizontal divider reaching the strip tees in too.
    let both = p.compose_buffer(rect, SidebarRule::Trailing, &[1, 6]);
    assert_eq!(both[(19, 1)].symbol(), "├");
    assert_eq!(both[(19, 6)].symbol(), "├");
    assert_eq!(both[(19, 5)].symbol(), "│");
}

/// Headers carry their count and every row's secondary text lands on
/// the same right edge, so the strip reads as a table.
#[test]
fn headers_count_and_secondary_text_share_one_right_edge() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_agents(vec![
        agent(0, "editor", "claude", AgentMetaState::Blocked),
        agent(1, "build", "codex", AgentMetaState::Done),
    ]);
    p.set_roster(vec![active_roster()]);
    p.set_windows(vec![win_branch("editor", true, "main")]);
    let rect = r(0, 0, 30, 16);
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    let rows: Vec<String> = (0..rect.h).map(|y| row_text(&buf, rect, y)).collect();
    assert!(
        rows[0].starts_with(" Agents") && rows[0].ends_with("2 "),
        "{rows:?}"
    );
    assert!(rows[2].ends_with("claude "), "{rows:?}");
    assert!(rows[3].ends_with("codex "), "{rows:?}");
    let window = rows
        .iter()
        .find(|row| row.contains("editor") && row.contains("main"))
        .expect("window row");
    assert!(window.ends_with("main "), "branch flush right: {window:?}");
}
