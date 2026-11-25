//! Composited chrome and floating-overlay tests through the PTY-probe oracle.

use super::*;

// ---- composited frames through the PTY-probe oracle ------------------------

const PROBE_SIDEBAR_W: u16 = 20;
/// Distinctive sidebar window label and host; neither appears in pane text.
const PROBE_WINDOW: &str = "w1-agent";
pub(super) const PROBE_SESSION: &str = "probe-sess";
pub(super) const PROBE_PANE_TEXT: &str = "PANE-BASE";

/// Replay `bytes` into a fresh terminal and project it to trimmed rows.
fn probe_rows(bytes: &[u8], (cols, rows): (u16, u16)) -> Vec<String> {
    crate::attach::pane_state::probe_frame(bytes, (cols, rows))
        .cells
        .chunks(usize::from(cols))
        .map(|row| {
            row.iter()
                .map(|c| c.grapheme.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

pub(super) fn probe_window(name: &str, active: bool) -> WindowInfo {
    WindowInfo {
        name: name.to_owned(),
        active,
        ..WindowInfo::default()
    }
}

fn probe_sidebar(windows: &[WindowInfo]) -> SidebarPainter {
    let mut painter = SidebarPainter::new(crate::render::theme::Theme::default());
    painter.set_roster(vec![crate::render::chrome::sidebar::SessionRosterEntry {
        name: PROBE_SESSION.to_owned(),
        host: "probe-host".to_owned(),
        active: true,
        selectable: true,
        ..Default::default()
    }]);
    painter.set_windows(windows.to_vec());
    painter
}

/// A full frame with the SHIPPED status bar (from `default.toml`), so the
/// real lineup, slot policy, and shrink ladders are exercised together.
pub(super) fn shipped_frame_rows(
    view: (u16, u16),
    windows: &[WindowInfo],
    sidebar: Option<SidebarReservation>,
) -> Vec<String> {
    let (cols, rows) = view;
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let cfg =
        phux_config::parse_with_defaults("", Path::new("/nonexistent/c.toml")).expect("defaults");
    let mut status_bar = crate::settings::compose_status_bar(&cfg, &[])
        .expect("the shipped status lineup must build")
        .expect("the shipped lineup is non-empty");
    status_bar.set_windows(windows.to_vec());
    let pane_rows = rows.saturating_sub(1);
    let mut panes = HashMap::from([(
        id.clone(),
        PaneSlot::new_with_size(cols, pane_rows).expect("slot"),
    )]);
    let kernel = published_test_kernel(&id, cols, pane_rows, PROBE_PANE_TEXT.as_bytes());
    let mut sidebar_painter = probe_sidebar(windows);
    let mut out: Vec<u8> = Vec::new();
    let mut chrome = ChromeCtx {
        sidebar,
        status_bar: Some(&mut status_bar),
        sidebar_painter: Some(&mut sidebar_painter),
        ..ChromeCtx::bare(view, "phux")
    };
    paint_full_frame(
        &mut out,
        &workspace.render_group(None).expect("layout"),
        &mut panes,
        &kernel,
        Some(&id),
        &mut chrome,
    );
    probe_rows(&out, view)
}

/// The shipped bar across widths: roomy shows tabs and session with no
/// teaching strip; phone-sized swaps the clock for a `switch` chip; narrower
/// still collapses far tabs behind `›` while keeping the active tab whole.
#[test]
fn shipped_frame_bar_degrades_gracefully_with_width() {
    let windows = [
        probe_window("zsh", false),
        probe_window("nvim", true),
        probe_window("server", false),
        probe_window("logs", false),
    ];
    let roomy = shipped_frame_rows((100, 12), &windows[..3], None);
    let bar = &roomy[0];
    assert!(bar.contains(" 1 nvim ") && bar.contains("phux"), "{bar:?}");
    for absent in ["s Sessions", "S Settings", "agents"] {
        assert!(!bar.contains(absent), "{bar:?}");
    }
    assert!(roomy.join("\n").contains(PROBE_PANE_TEXT));

    for (view, collapsed) in [((46, 12), false), ((36, 10), true)] {
        let rows = shipped_frame_rows(view, &windows, None);
        let bar = &rows[0];
        assert!(bar.contains(" 1 nvim "), "active tab whole: {bar:?}");
        assert!(bar.contains("agents"), "{bar:?}");
        assert!(!bar.contains("Space palette"), "{bar:?}");
        assert!(bar.chars().count() <= usize::from(view.0), "{bar:?}");
        if collapsed {
            assert!(
                bar.contains('\u{203a}') && !bar.contains("3 logs"),
                "{bar:?}"
            );
        }
    }
}

/// One `paint_active_overlay` frame over a left sidebar, with the sidebar
/// painter threaded when `with_painter`, projected to strip columns and rows.
fn overlay_frame(overlay: Box<dyn RenderOverlay>, with_painter: bool) -> (Vec<String>, String) {
    const VIEW: (u16, u16) = (80, 24);
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let sidebar = Some(SidebarReservation {
        edge: SidebarEdge::Left,
        width: PROBE_SIDEBAR_W,
    });
    let pane_cols = VIEW.0 - PROBE_SIDEBAR_W;
    let mut panes = HashMap::from([(
        id.clone(),
        PaneSlot::new_with_size(pane_cols, VIEW.1).expect("slot"),
    )]);
    let kernel = published_test_kernel(&id, pane_cols, VIEW.1, PROBE_PANE_TEXT.as_bytes());
    let mut sidebar_painter = probe_sidebar(&[probe_window(PROBE_WINDOW, true)]);
    let mut overlays = OverlayState::new();
    overlays.push(overlay);
    let mut out: Vec<u8> = Vec::new();
    let mut chrome = ChromeCtx {
        sidebar,
        sidebar_painter: with_painter.then_some(&mut sidebar_painter),
        ..ChromeCtx::bare(VIEW, "probe")
    };
    paint_active_overlay(
        &mut out,
        &overlays,
        workspace.render_group(None).as_deref(),
        &mut panes,
        &kernel,
        Some(&id),
        &mut chrome,
    );
    let rows = probe_rows(&out, VIEW);
    let strip = rows
        .iter()
        .map(|r| {
            r.chars()
                .take(usize::from(PROBE_SIDEBAR_W))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (rows, strip)
}

/// The command palette, as the dispatcher builds it.
pub(super) fn palette_overlay() -> Box<dyn RenderOverlay> {
    let item = |action: &str| {
        SelectItem::new(
            action,
            crate::attach::resolved(action, std::collections::BTreeMap::new()),
        )
    };
    let theme = crate::render::Theme::default();
    Box::new(SelectList::new(
        "command palette",
        vec![item("detach"), item("new-window")],
        &theme,
    ))
}

/// Opening the palette must not blank the sidebar: the floating-modal base
/// frame repaints the strip, then centers the modal in the pane area.
#[test]
fn command_palette_keeps_sidebar_visible() {
    let (_, blank) = overlay_frame(palette_overlay(), false);
    assert!(
        !blank.contains(PROBE_WINDOW) && !blank.contains(PROBE_SESSION),
        "{blank}"
    );

    let (rows, strip) = overlay_frame(palette_overlay(), true);
    let all = rows.join("\n");
    assert!(
        strip.contains(PROBE_WINDOW) && strip.contains(PROBE_SESSION),
        "{all}"
    );
    assert!(
        all.contains("command palette") && all.contains(PROBE_PANE_TEXT),
        "{all}"
    );
    assert!(
        !strip.contains('╭') && !strip.contains('╰'),
        "modal intruded into the strip:\n{strip}"
    );
}

/// Every floating overlay kind (fleet, which-key, prompt, toast) shares the
/// base-frame path and must keep the sidebar too.
#[test]
fn all_floating_overlays_keep_sidebar_visible() {
    let theme = crate::render::Theme::default();
    let wk_cfg = KeybindingsCfg {
        prefix_table: std::iter::once((
            "d".to_owned(),
            phux_config::Action::Bare("detach".to_owned()),
        ))
        .collect(),
        ..KeybindingsCfg::default()
    };
    let fleet_items = crate::attach::fleet::local_fleet_items(
        &ArrangementDoc::single(ResourceId::local(1)),
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(!fleet_items.iter().all(SelectItem::is_header));
    let overlays: Vec<(&str, Box<dyn RenderOverlay>)> = vec![
        (
            "agent-fleet",
            Box::new(
                SelectList::new("agent fleet", fleet_items, &theme)
                    .with_live_key(crate::attach::fleet::FLEET_LIVE_KEY),
            ),
        ),
        (
            "which-key",
            Box::new(crate::render::overlay::WhichKeyOverlay::from_config(
                &wk_cfg, &theme,
            )),
        ),
        (
            "prompt",
            Box::new(crate::render::overlay::PromptOverlay::new(
                "rename tab",
                "rename-window",
                "name",
                "1",
                &theme,
            )),
        ),
        (
            "toast",
            Box::new(crate::render::overlay::ToastOverlay::new(
                "notice",
                vec!["a line".to_owned()],
                &theme,
            )),
        ),
    ];
    for (label, overlay) in overlays {
        let (rows, strip) = overlay_frame(overlay, true);
        assert!(
            strip.contains(PROBE_WINDOW) && strip.contains(PROBE_SESSION),
            "{label}:\n{}",
            rows.join("\n")
        );
    }
}
