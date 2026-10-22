//! Window rows: labels, markers, repaint, and the drop marker.

use super::*;

/// phux-l96p.8 fix pass: a row must advance the terminal's cursor
/// exactly `rect.w` columns.
///
/// The strip emits each row as one uninterrupted run from its own
/// `CUP`, so its column budget is only as good as its idea of how
/// wide each glyph is. A DOUBLE-WIDTH character advances two columns
/// and ratatui leaves the cell it spills into empty; writing a space
/// there advanced the row one column too far per wide character, and
/// a CJK window name walked the whole strip out of its reserved
/// columns and over the panes beside it — libghostty's cells
/// (ADR-0020).
#[test]
fn a_wide_window_name_stays_inside_the_strip() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_roster(vec![active_roster()]);
    assert!(p.set_windows(vec![win("日本語のペイン名前がとても長い", true)]));
    let rect = r(0, 0, 20, 12);
    let painted = paint_to_string(&mut p, rect);
    // Each emitted row is one run from one CUP, so the emitted
    // GLYPH widths per row must total the strip's width exactly —
    // one column too many and the next row starts inside a pane.
    let rows = rows_of(&painted);
    assert_eq!(rows.len(), usize::from(rect.h), "one run per strip row");
    for row in rows {
        assert_eq!(
            crate::render::display_width(&row),
            usize::from(rect.w),
            "row {row:?} does not advance the cursor exactly {} columns",
            rect.w
        );
    }
}

#[test]
fn renders_each_window_label() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_roster(vec![active_roster()]);
    p.set_windows(vec![win("editor", false), win("shell", true)]);
    let raw = paint_to_string(&mut p, r(0, 0, 20, 14));
    let plain = strip_ansi(&raw);
    assert!(plain.contains("editor"), "first tab label: {plain:?}");
    assert!(plain.contains("shell"), "second tab label: {plain:?}");
    assert!(plain.contains(SPACES_HEADER), "sessions header: {plain:?}");
    // The active window gets the filled status dot.
    assert!(plain.contains('●'), "active dot missing: {plain:?}");
    assert!(plain.contains('○'), "inactive dot missing: {plain:?}");
    // Separator rule present.
    assert!(plain.contains('│'), "separator missing: {plain:?}");
}

#[test]
fn unchanged_repaint_is_a_no_op() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_roster(vec![active_roster()]);
    p.set_windows(vec![win("a", true)]);
    let rect = r(0, 0, 16, 12);
    assert!(
        !paint_to_string(&mut p, rect).is_empty(),
        "first paint emits"
    );
    // Same inputs ⇒ cached ⇒ nothing emitted.
    assert!(
        paint_to_string(&mut p, rect).is_empty(),
        "unchanged repaint must emit nothing"
    );
    // A window change invalidates the cache.
    p.set_windows(vec![win("b", true)]);
    assert!(
        !paint_to_string(&mut p, rect).is_empty(),
        "changed windows must re-emit"
    );
    // Branches that fit repaint; omitted long branches stay invisible.
    p.set_agents(vec![agent(0, "b", "claude", AgentMetaState::Idle)]);
    paint_to_string(&mut p, rect);
    p.set_windows(vec![win_branch("b", true, "main")]);
    assert!(!paint_to_string(&mut p, rect).is_empty());
    p.set_windows(vec![win_branch("b", true, "a-branch-too-long-to-fit")]);
    paint_to_string(&mut p, rect);
    p.set_windows(vec![win_branch(
        "b",
        true,
        "another-branch-too-long-to-fit",
    )]);
    assert!(
        paint_to_string(&mut p, rect).is_empty(),
        "hidden branch context must not re-emit identical cells"
    );
    assert!(!paint_to_string(&mut p, Rect { h: 14, ..rect }).is_empty());
}

#[test]
fn host_update_emits_only_its_row_and_invalidation_restores_every_row() {
    let mut p = SidebarPainter::new(Theme::default());
    // Band off: this test is about the roster rows.
    p.set_agents_share(0);
    let rect = r(7, 2, 36, 24);
    let peer = SessionRosterEntry {
        host: "xps".to_owned(),
        ..roster("peer", 0, 0, 0)
    };
    p.set_windows(vec![win("editor", true)]);
    p.set_roster(vec![active_roster(), peer.clone()]);
    let full = paint_to_string(&mut p, rect);
    p.set_roster(vec![
        SessionRosterEntry {
            host: "devbox".to_owned(),
            ..active_roster()
        },
        peer,
    ]);
    let changed = paint_to_string(&mut p, rect);
    assert_eq!(
        changed.matches('H').count(),
        1,
        "one CUP for the changed host row"
    );
    // Sessions tops a strip with no agents; the host line is row 1.
    assert!(changed.starts_with("\x1b[4;8H"));
    assert!(strip_ansi(&changed).contains("devbox"));
    assert!(!changed.contains("editor"));
    assert!(
        changed.len() * 8 < full.len(),
        "one row costs much less than the whole strip"
    );
    p.invalidate();
    assert_eq!(paint_to_string(&mut p, rect).matches('H').count(), 24);
    assert!(paint_to_string(&mut p, rect).is_empty());
    assert_eq!(
        paint_to_string(&mut p, Rect { x: 8, ..rect })
            .matches('H')
            .count(),
        24
    );
}

#[test]
fn unicode_roster_counts_align_and_selection_owns_its_gutters() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_roster(vec![active_roster()]);
    p.set_windows(vec![win_branch("编辑器", true, "cafe\u{301}")]);
    let rect = r(0, 0, 36, 12);
    // The active tab's row, under the fixed Agents panel.
    let active_row = u16::try_from(
        row_model(p.counts(), rect.h)
            .iter()
            .position(|row| *row == SidebarRow::WindowName(0))
            .expect("the active window row"),
    )
    .expect("row fits");
    let b = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    for y in [0, 1] {
        assert_eq!(b[(0, y)].bg, p.theme.surface);
        assert_eq!(b[(34, y)].bg, p.theme.surface);
        assert_eq!(b[(35, y)].bg, p.theme.surface);
    }
    assert_eq!(b[(0, active_row)].bg, p.theme.selection_bg);
    assert_eq!(b[(34, active_row)].bg, p.theme.selection_bg);
    assert_eq!(b[(35, active_row)].bg, p.theme.surface);
    for name in ["构建工具", "cafe\u{301}", "build"] {
        let line = p.roster_line(&roster(name, 1, 2, 0), 30);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(display_width(&text), 30);
        assert!(text.ends_with("●1"), "working is not counted: {text:?}");
    }
}

#[test]
fn compact_attention_rows_keep_same_session_agents_distinguishable() {
    let p = SidebarPainter::new(Theme::default());
    let text = |name| {
        let mut e = agent(0, "work", name, AgentMetaState::Blocked);
        e.session = Some("development".to_owned());
        p.agent_line(&e, 25)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    };
    assert!(text("claude").contains("claude"));
    assert!(text("codex").contains("codex"));
    assert_ne!(text("claude"), text("codex"));
}

/// An asking window (ADR-0035) carries `!`; a retained exited pane
/// (ADR-0124) carries `x<status>`. Either flip re-emits.
#[test]
fn window_markers_for_attention_and_retained_exit() {
    for (marked, text) in [
        (win_attention("shell", false), "shell !"),
        (win_exited("shell", false, "3"), "shell x3"),
    ] {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        p.set_windows(vec![win("editor", true), win("shell", false)]);
        let rect = r(0, 0, 20, 14);
        let plain = strip_ansi(&paint_to_string(&mut p, rect));
        assert!(
            !plain.contains("shell !") && !plain.contains(" x"),
            "{plain:?}"
        );
        assert!(p.set_windows(vec![win("editor", true), marked]));
        let plain = strip_ansi(&paint_to_string(&mut p, rect));
        assert!(plain.contains(text), "{plain:?}");
    }
}

/// An identical window list reports no change (the agent-event chrome
/// path gates its repaint on this).
#[test]
fn set_windows_reports_change_only_on_difference() {
    let mut p = SidebarPainter::new(Theme::default());
    assert!(p.set_windows(vec![win("a", true)]));
    assert!(!p.set_windows(vec![win("a", true)]));
    assert!(p.set_windows(vec![win_attention("a", true)]));
    // Branch changes still update projection equality even though compact
    // window rows omit branch context and emit no changed cells.
    assert!(p.set_windows(vec![win_branch("a", true, "main")]));
    assert!(p.set_windows(vec![win_branch("a", true, "feature")]));
    // Same contract for the agents section — a state
    // flip (idle -> working) must repaint.
    let done = agent(0, "a", "claude", AgentMetaState::Done);
    assert!(p.set_agents(vec![done.clone()]));
    assert!(!p.set_agents(vec![done]));
    assert!(p.set_agents(vec![agent(0, "a", "claude", AgentMetaState::Blocked)]));
    // Under `all` every lifecycle reaches the list, so each flip reports.
    assert!(p.set_agents(vec![agent(0, "a", "claude", AgentMetaState::Working)]));
    assert!(p.set_agents(vec![agent(0, "a", "claude", AgentMetaState::Idle)]));
    // A filter that moved reports too; the same filter again does not.
    let need = AgentFilter {
        scope: AgentScope::Attention,
        host: HostFilter::Any,
    };
    assert!(p.set_agent_filter(need.clone()));
    assert!(!p.set_agent_filter(need));
}

/// Tabs expand compactly beneath the current session; one machine
/// paints no host line.
#[test]
fn windows_nest_under_active_session_without_extra_sections() {
    let mut p = SidebarPainter::new(Theme::default());
    // Band off: this test is about window nesting under the session.
    p.set_agents_share(0);
    p.set_roster(vec![active_roster(), roster("peer", 0, 0, 0)]);
    p.set_windows(vec![
        win_branch("phux", true, "wave2/herdr"),
        win("scratch", false),
    ]);
    let rect = r(0, 0, 20, 18);
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert!(row_text(&buf, rect, 0).contains(SPACES_HEADER));
    assert!(row_text(&buf, rect, 1).contains("development"));
    assert!(row_text(&buf, rect, 2).starts_with("   ● phux"));
    assert!(row_text(&buf, rect, 3).starts_with("   ○ scratch"));
    assert!(row_text(&buf, rect, 4).contains("peer"));
    let text = strip_text(&p, rect);
    assert!(!text.contains("mini"), "{text}");
    assert!(!text.contains("wave2/herdr"));
}

/// A live window-row drag paints the insertion marker on
/// the pointed-at row and clears it when the index is dropped.
#[test]
fn window_drop_marker_follows_the_index_and_clears() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_roster(vec![active_roster()]);
    p.set_windows(vec![win("phux", true), win("scratch", false)]);
    let rect = r(0, 0, 20, 18);
    let model = row_model(p.counts(), rect.h);
    let y0 = window_row_y(&model, 0);
    let y1 = window_row_y(&model, 1);
    let unmarked = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert!(
        !row_reversed(&unmarked, rect, y0) && !row_reversed(&unmarked, rect, y1),
        "no drag, no marker"
    );
    assert!(p.set_drop_index(Some(1)));
    assert!(!p.set_drop_index(Some(1)));
    let marked = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert!(
        row_reversed(&marked, rect, y1),
        "drop index 1 must reverse that window row"
    );
    assert!(
        !row_reversed(&marked, rect, y0),
        "the other window row stays unmarked"
    );
    assert!(p.set_drop_index(None));
    let cleared = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    assert!(
        !row_reversed(&cleared, rect, y1),
        "clearing the index must drop the marker"
    );
}

fn window_row_y(model: &[SidebarRow], index: usize) -> u16 {
    model
        .iter()
        .position(|row| *row == SidebarRow::WindowName(index))
        .and_then(|y| u16::try_from(y).ok())
        .expect("window row is on the strip")
}

fn row_reversed(buf: &Buffer, rect: Rect, y: u16) -> bool {
    (0..rect.w.saturating_sub(1)).any(|x| buf[(x, y)].modifier.contains(Modifier::REVERSED))
}
