//! Action-dispatch tests: `run_action` arms, pickers, attention
//! navigation, and session flows.
#![allow(clippy::expect_used, reason = "tests")]

//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_TERMINAL` reply.

use std::collections::{HashMap, HashSet};

use phux_protocol::TerminalId;
use phux_protocol::wire::frame::FrameKind;

use crate::attach::connection::Connection;
use crate::attach::focus::FocusHistory;
use crate::attach::pane_state::{AttentionNavigation, PaneSlot};
use crate::attach::plugin_panes::{HostedPlacement, PluginPaneEntry};
use crate::layout::{SplitDir, Workspace};
use crate::predict::PredictionState;
use crate::render::overlay::{OverlayState, PromptOverlay};
use crate::render::{ChromeBreakpoints, Theme};

use std::collections::BTreeMap;

use crate::render::overlay::CopyModeOverlay;

use super::args::*;
use super::ctx::*;
use super::dispatch::*;
use super::effects::*;
use super::pickers::*;
use super::run_action::*;
use super::test_support::*;

#[test]
fn soft_kill_input_frames_emits_exit_newline_sequence() {
    let frames = soft_kill_input_frames(&tid(7));
    assert_eq!(frames.len(), 5, "expected e/x/i/t/Enter");
    // Each frame is INPUT_KEY targeting tid(7).
    for f in &frames {
        match f {
            FrameKind::InputKey { terminal_id, .. } => {
                assert_eq!(terminal_id, &tid(7));
            }
            other => panic!("expected InputKey, got {other:?}"),
        }
    }
    // First four are printable letters with text="e".."t".
    let expected_text = ["e", "x", "i", "t"];
    for (i, want) in expected_text.iter().enumerate() {
        match &frames[i] {
            FrameKind::InputKey { event, .. } => {
                assert_eq!(
                    event.text.as_deref(),
                    Some(*want),
                    "frame {i}: text mismatch",
                );
            }
            _ => unreachable!(),
        }
    }
    // Last frame is Enter (no text).
    match &frames[4] {
        FrameKind::InputKey { event, .. } => {
            assert_eq!(event.key, phux_protocol::input::key::PhysicalKey::Enter);
            assert_eq!(event.text, None);
        }
        _ => unreachable!(),
    }
}

#[test]
fn split_dir_arg_parses_horizontal_and_vertical() {
    use phux_config::keybind::ResolvedAction;
    // `direction` names the divider orientation, not the split axis:
    // "horizontal" divider ⇒ stacked panes ⇒ SplitDir::Vertical;
    // "vertical" divider ⇒ side-by-side panes ⇒ SplitDir::Horizontal.
    let mut h = ResolvedAction {
        action: "split-pane".to_owned(),
        args: std::collections::BTreeMap::new(),
    };
    h.args.insert(
        "direction".to_owned(),
        toml::Value::String("horizontal".into()),
    );
    assert_eq!(split_dir_arg(&h), Some(SplitDir::Vertical));

    let mut v = ResolvedAction {
        action: "split-pane".to_owned(),
        args: std::collections::BTreeMap::new(),
    };
    v.args.insert(
        "direction".to_owned(),
        toml::Value::String("vertical".into()),
    );
    assert_eq!(split_dir_arg(&v), Some(SplitDir::Horizontal));

    let mut bogus = ResolvedAction {
        action: "split-pane".to_owned(),
        args: std::collections::BTreeMap::new(),
    };
    bogus.args.insert(
        "direction".to_owned(),
        toml::Value::String("diagonal".into()),
    );
    assert_eq!(split_dir_arg(&bogus), None);
}

#[test]
fn focused_pane_rect_tracks_rendered_pane_bounds() {
    use crate::layout::{LayoutNode, LayoutState, Rect, WindowState, split_at};

    let tree = split_at(
        &LayoutNode::Leaf(tid(1)),
        &tid(1),
        &tid(2),
        SplitDir::Horizontal,
        0.5,
    )
    .unwrap();
    let workspace = Workspace {
        windows: vec![WindowState {
            name: "1".to_owned(),
            state: LayoutState {
                tree: Some(tree),
                focus: Some(tid(2)),
            },
        }],
        active: 0,
    };

    let split_rect = focused_pane_rect_for(
        &workspace,
        None,
        Some(&tid(2)),
        (80, 24),
        Some(crate::render::chrome::status_bar::Position::Bottom),
        None,
    );
    // Row 0 is the pane-grid rail (phux-l96p.8); panes start at row 1.
    assert_eq!(split_rect.y, 1);
    assert_eq!(
        split_rect.h, 22,
        "neither the status-bar row nor the pane rail is copy-mode content"
    );
    assert_eq!(split_rect.x + split_rect.w, 80);
    assert!(
        split_rect.w < 80,
        "split pane must not inherit the outer viewport width"
    );
    assert_ne!(
        split_rect,
        Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 23
        }
    );

    let zoomed = tid(2);
    let zoomed_rect = focused_pane_rect_for(
        &workspace,
        Some(&zoomed),
        Some(&tid(2)),
        (80, 24),
        Some(crate::render::chrome::status_bar::Position::Bottom),
        None,
    );
    // A zoomed pane takes the whole content rect — which is still the
    // content rect: the rail is chrome, and zoom does not reclaim it.
    assert_eq!(
        zoomed_rect,
        Rect {
            x: 0,
            y: 1,
            w: 80,
            h: 22
        }
    );
}

/// Run `action` against `workspace`, returning the resulting effects.
fn run(action: &phux_config::keybind::ResolvedAction, workspace: &mut Workspace) -> ActionEffects {
    run_with_last(action, workspace, None)
}

/// [`run`] against a server that did NOT advertise
/// `ServerFeature::SpawnInitialSize` (phux-a5xj).
fn run_without_spawn_size_support(
    action: &phux_config::keybind::ResolvedAction,
    workspace: &mut Workspace,
) -> ActionEffects {
    run_with_last_and_spawn_size(action, workspace, None, false)
}

fn run_with_last(
    action: &phux_config::keybind::ResolvedAction,
    workspace: &mut Workspace,
    last_focused: Option<TerminalId>,
) -> ActionEffects {
    run_with_last_and_spawn_size(action, workspace, last_focused, true)
}

fn run_with_last_and_spawn_size(
    action: &phux_config::keybind::ResolvedAction,
    workspace: &mut Workspace,
    last_focused: Option<TerminalId>,
    spawn_initial_size_supported: bool,
) -> ActionEffects {
    let mut next_request_id = 100;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut overlays = OverlayState::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut reload_request = false;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    let mut engine_kernel = test_engine_kernel();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: None,
        focus_history: last_focused.map_or_else(FocusHistory::default, FocusHistory::with_previous),
        workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        input_replay: None,
        spawn_initial_size_supported,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };
    let focused = ctx.workspace.active_window().and_then(|w| w.focus.clone());
    run_action(action, &mut ctx, focused.as_ref(), &HashMap::new())
}

#[test]
fn reload_config_action_raises_the_reload_effect() {
    // phux-foz.5: the arm only raises the effect — the driver owns
    // the actual re-read + swap (the ctx borrows the state to
    // replace). No layout mutation, no bell, no frames.
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("reload-config"), &mut workspace);
    assert!(effects.reload_config, "reload-config must raise the effect");
    assert!(!effects.layout_mutated);
    assert!(!effects.bell);
    assert!(effects.kill_frames.is_empty());
}

#[test]
fn new_window_parks_pending_and_emits_spawn() {
    let mut workspace = Workspace::single(tid(1)); // window "1"
    let effects = run(&bare_action("new-window"), &mut workspace);
    let (_req, pending, frame) = effects
        .spawn_window
        .expect("new-window should park a PendingWindow + SPAWN");
    // Default name skips the in-use "1".
    assert_eq!(pending.name, "2");
    assert!(matches!(frame, FrameKind::SpawnTerminal { .. }));
    // No synchronous workspace mutation — the window opens on reply.
    assert_eq!(workspace.windows.len(), 1);
}

#[test]
fn kill_window_emits_one_soft_kill_sequence_per_leaf() {
    use crate::layout::{LayoutNode, LayoutState, SplitDir, WindowState, split_at};
    // Active window with three leaves: ((1|2)/3).
    let tree = split_at(
        &LayoutNode::Leaf(tid(1)),
        &tid(1),
        &tid(2),
        SplitDir::Horizontal,
        0.5,
    )
    .unwrap();
    let tree = split_at(&tree, &tid(2), &tid(3), SplitDir::Vertical, 0.5).unwrap();
    let mut workspace = Workspace {
        windows: vec![WindowState {
            name: "1".to_owned(),
            state: LayoutState {
                tree: Some(tree),
                focus: Some(tid(1)),
            },
        }],
        active: 0,
    };
    let effects = run(&bare_action("kill-window"), &mut workspace);
    // 3 leaves x 5 frames (e/x/i/t/Enter) each.
    assert_eq!(effects.kill_frames.len(), 15);
    // phux-i0e8.2.2: every targeted leaf is marked as an expected
    // close so the resulting TERMINAL_CLOSEDs stay notice-silent.
    assert_eq!(effects.expected_closes, vec![tid(1), tid(2), tid(3)]);
    // No synchronous removal — TerminalClosed folds + prunes.
    assert_eq!(workspace.windows.len(), 1);
}

/// phux-i0e8.2.2: `kill-pane` marks its own target as an expected
/// close alongside the soft-kill frames.
#[test]
fn kill_pane_marks_the_focused_pane_as_expected_close() {
    let mut workspace = Workspace::single(tid(7));
    let effects = run(&bare_action("kill-pane"), &mut workspace);
    assert!(!effects.kill_frames.is_empty());
    assert_eq!(effects.expected_closes, vec![tid(7)]);
}

#[test]
fn next_window_switches_active_clears_predict_no_metadata() {
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("2".to_owned(), tid(2));
    workspace.select(0);
    let effects = run(&bare_action("next-window"), &mut workspace);
    assert_eq!(workspace.active, 1);
    assert!(effects.layout_mutated);
    assert!(effects.clear_predict);
    assert!(!effects.set_metadata, "window switch is per-client");
    assert_eq!(effects.set_focus, Some(tid(2)));
}

#[test]
fn last_pane_dispatch_jumps_across_windows_and_toggles() {
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("2".to_owned(), tid(2));
    workspace.select(0);

    let action = bare_action("last-pane");
    let effects = run_with_last(&action, &mut workspace, Some(tid(2)));
    assert_eq!(workspace.active, 1);
    assert_eq!(
        workspace.active_window().and_then(|w| w.focus.clone()),
        Some(tid(2))
    );
    assert_eq!(effects.set_focus, Some(tid(2)));
    assert!(effects.clear_predict);
    assert!(!effects.set_metadata, "focus MRU is client-local");
    let mut focused = Some(tid(1));
    let mut history = FocusHistory::with_previous(tid(2));
    apply_focus_transition(
        &mut history,
        &mut focused,
        effects.set_focus.expect("dispatch target"),
    );
    assert_eq!(history.previous(), Some(&tid(1)));

    // Feed the recorded pane back through dispatch + the same apply path:
    // repeated last-pane genuinely toggles and repairs the MRU to pane 2.
    let effects = run_with_last(&action, &mut workspace, Some(tid(1)));
    assert_eq!(workspace.active, 0);
    apply_focus_transition(
        &mut history,
        &mut focused,
        effects.set_focus.expect("toggle target"),
    );
    assert_eq!(focused, Some(tid(1)));
    assert_eq!(history.previous(), Some(&tid(2)));
}

// ---------------------------------------------------------------------
// phux-a5xj — the dispatcher stamps the tile onto the spawn
// ---------------------------------------------------------------------

fn spawn_initial_size_of(frame: &FrameKind) -> Option<(u16, u16)> {
    let FrameKind::SpawnTerminal { initial_size, .. } = frame else {
        panic!("expected SpawnTerminal, got {frame:?}");
    };
    *initial_size
}

/// A `split-pane` must name the tile the new leaf is about to occupy, so
/// the server bootstraps the pane there instead of at 80x24 and then
/// being told the truth by a resize that throws the checkpoint away.
///
/// The 80x24 viewport tiles to an 80x23 content rect (row 0 is the
/// pane-grid rail, phux-l96p.8); a horizontal split spends one divider
/// column, leaving 79 to share 40/39 — so the new (right-hand) leaf is
/// 39x23. Asserting the exact number, not merely "some size", is what
/// makes this a regression guard rather than a smoke test.
#[test]
fn split_pane_spawn_carries_the_new_leafs_tile() {
    let mut workspace = Workspace::single(tid(1));
    let mut action = bare_action("split-pane");
    action.args.insert(
        "direction".to_owned(),
        toml::Value::String("vertical".into()),
    );
    let effects = run(&action, &mut workspace);
    let (_req, _pending, frame) = effects.spawn_terminal.expect("split parks a SPAWN");
    assert_eq!(spawn_initial_size_of(&frame), Some((39, 23)));
}

/// A `new-window` seeds a window holding one leaf, so the pane fills the
/// whole content rect.
#[test]
fn new_window_spawn_carries_the_full_content_rect() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("new-window"), &mut workspace);
    let (_req, _pending, frame) = effects.spawn_window.expect("new-window parks a SPAWN");
    assert_eq!(spawn_initial_size_of(&frame), Some((80, 23)));
}

/// Against a server that never advertised the capability the field stays
/// absent, so the frame is byte-identical to what that server has always
/// decoded (ADR-0061: a client MUST NOT depend on unadvertised surface).
#[test]
fn spawn_omits_initial_size_when_the_server_did_not_advertise_it() {
    let mut workspace = Workspace::single(tid(1));
    let mut action = bare_action("split-pane");
    action.args.insert(
        "direction".to_owned(),
        toml::Value::String("vertical".into()),
    );
    let effects = run_without_spawn_size_support(&action, &mut workspace);
    let (_req, _pending, frame) = effects.spawn_terminal.expect("split parks a SPAWN");
    assert_eq!(spawn_initial_size_of(&frame), None);

    let mut workspace = Workspace::single(tid(1));
    let effects = run_without_spawn_size_support(&bare_action("new-window"), &mut workspace);
    let (_req, _pending, frame) = effects.spawn_window.expect("new-window parks a SPAWN");
    assert_eq!(spawn_initial_size_of(&frame), None);
}

#[test]
fn last_pane_without_history_bells_without_mutation() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("last-pane"), &mut workspace);
    assert!(effects.bell);
    assert!(!effects.layout_mutated);
    assert!(effects.set_focus.is_none());
}

#[test]
fn next_window_single_window_is_noop() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("next-window"), &mut workspace);
    assert_eq!(workspace.active, 0);
    assert!(!effects.layout_mutated);
    assert!(!effects.clear_predict);
}

#[test]
fn select_window_jumps_to_index() {
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("2".to_owned(), tid(2));
    workspace.add_window("3".to_owned(), tid(3)); // active = 2
    let mut action = bare_action("select-window");
    action
        .args
        .insert("index".to_owned(), toml::Value::Integer(0));
    let effects = run(&action, &mut workspace);
    assert_eq!(workspace.active, 0);
    assert!(effects.layout_mutated);
    assert_eq!(effects.set_focus, Some(tid(1)));
}

#[test]
fn select_window_out_of_range_is_noop() {
    let mut workspace = Workspace::single(tid(1)); // only index 0
    let mut action = bare_action("select-window");
    action
        .args
        .insert("index".to_owned(), toml::Value::Integer(5));
    let effects = run(&action, &mut workspace);
    assert_eq!(workspace.active, 0);
    assert!(!effects.layout_mutated);
}

#[test]
fn select_window_missing_index_bells() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("select-window"), &mut workspace);
    assert!(effects.bell);
    assert!(!effects.layout_mutated);
}

/// phux-x2hm: a multi-pane window can zoom — `toggle-zoom` requests the
/// driver-side flip (`toggle_zoom`) plus a repaint (`layout_mutated`),
/// without mutating the real tree or bell-ing.
#[test]
fn toggle_zoom_on_multi_pane_window_requests_toggle() {
    use crate::layout::{LayoutState, WindowState, split_at};
    let tree = split_at(
        &crate::layout::LayoutNode::Leaf(tid(1)),
        &tid(1),
        &tid(2),
        crate::layout::SplitDir::Horizontal,
        0.5,
    )
    .unwrap();
    let mut workspace = Workspace {
        windows: vec![WindowState {
            name: "1".to_owned(),
            state: LayoutState {
                tree: Some(tree),
                focus: Some(tid(1)),
            },
        }],
        active: 0,
    };
    let effects = run(&bare_action("toggle-zoom"), &mut workspace);
    assert!(effects.toggle_zoom, "multi-pane window may zoom");
    assert!(effects.layout_mutated, "zoom toggles drive a repaint");
    assert!(!effects.bell);
}

/// phux-x2hm: a single-pane window has nothing to zoom — `toggle-zoom`
/// bells (tmux parity) and does NOT request a toggle or repaint.
#[test]
fn toggle_zoom_on_single_pane_window_bells() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("toggle-zoom"), &mut workspace);
    assert!(effects.bell, "single-pane window cannot zoom");
    assert!(!effects.toggle_zoom);
    assert!(!effects.layout_mutated);
}

/// The root split's ratio, for asserting a resize actually moved it.
fn root_ratio(workspace: &Workspace) -> f32 {
    match workspace.active_window().unwrap().tree.as_ref().unwrap() {
        crate::layout::LayoutNode::Split { ratio, .. } => *ratio,
        other => panic!("expected root Split, got {other:?}"),
    }
}

/// Like [`two_pane_workspace`], but with a caller-chosen root ratio —
/// stands in for the workspace a peer's layout broadcast lands
/// (`server_frame`'s `is_layout_key` arm decodes and swaps it in
/// wholesale; a peer dragging the divider is the same shape as a
/// smaller `ratio` here).
fn two_pane_workspace_with_ratio(ratio: f32) -> Workspace {
    use crate::layout::{LayoutState, WindowState, split_at};
    let tree = split_at(
        &crate::layout::LayoutNode::Leaf(tid(1)),
        &tid(1),
        &tid(2),
        SplitDir::Horizontal,
        ratio,
    )
    .unwrap();
    Workspace {
        windows: vec![WindowState {
            name: "1".to_owned(),
            state: LayoutState {
                tree: Some(tree),
                focus: Some(tid(1)),
            },
        }],
        active: 0,
    }
}

/// phux-z6wt: a peer's layout broadcast can shrink the focused pane with
/// no SIGWINCH involved — `server_frame`'s `is_layout_key` arm decodes
/// the peer's `Workspace`, swaps it in wholesale, and returns
/// `FrameOutcome { layout_replaced: true, .. }`. PR #331 (phux-d26y)
/// only fanned the focused pane's new size out to overlays on the
/// SIGWINCH edge, so before this fix nothing on the `layout_replaced`
/// path called it: copy-mode kept clamping the selection into the pane
/// size it had when it opened. Stale-large strands the corner outside
/// the new, smaller grid, and the copy path resolves that corner
/// through `terminal.grid_ref(..).ok()?` — so `extract_selection_text`
/// returns `None` and Enter dismisses copy-mode having silently copied
/// nothing.
///
/// This drives `sync_overlays_to_focused_pane` — the driver helper the
/// `layout_replaced` block (and the SIGWINCH arm) both call — directly
/// against a workspace shaped like the "already reconciled" state
/// `server_frame` hands the driver, so the assertion exercises the
/// exact rect recomputation + fan-out the fix wires in, without needing
/// a two-client pty harness to produce the broadcast itself.
#[test]
fn layout_replace_reclaims_the_focused_pane_rect_for_overlays() {
    let viewport = (80, 24);

    // The wide workspace: a 50/50 split, so the focused left pane
    // (tid(1)) is roughly half the viewport.
    let wide = two_pane_workspace_with_ratio(0.5);
    let wide_pane = focused_pane_rect_for(&wide, None, Some(&tid(1)), viewport, None, None);

    // Copy-mode opens against that size and the selection is dragged to
    // the pane's bottom-right corner.
    let mut overlays = OverlayState::new();
    overlays.push(Box::new(CopyModeOverlay::new(
        wide_pane.h.saturating_sub(1),
        wide_pane.w.saturating_sub(1),
        wide_pane.w,
        wide_pane.h,
    )));

    // A peer shrinks the same divider hard to the left — this is the
    // workspace `server_frame` has already swapped in by the time the
    // driver's `layout_replaced` block runs, with no SIGWINCH anywhere
    // in the sequence.
    let narrow = two_pane_workspace_with_ratio(0.1);
    let narrow_pane = focused_pane_rect_for(&narrow, None, Some(&tid(1)), viewport, None, None);
    assert!(
        narrow_pane.w < wide_pane.w,
        "sanity: the shrink actually narrowed the focused pane",
    );

    // Precondition: the stale selection really is stranded outside the
    // narrower pane the peer just produced — this is the bug's exact
    // consequence, reproduced without touching any driver code.
    let stale = overlays
        .copy_selection()
        .expect("copy-mode retains its selection across the broadcast");
    assert!(
        stale.end_col >= narrow_pane.w,
        "sanity: the pre-fix corner ({stale:?}) must sit outside the \
             narrower pane ({narrow_pane:?}) for this test to mean anything",
    );

    // The fix: the driver's `layout_replaced` block calls this exact
    // helper with the already-swapped workspace.
    sync_overlays_to_focused_pane(
        &mut overlays,
        &narrow,
        None,
        Some(&tid(1)),
        viewport,
        None,
        None,
    );

    let fixed = overlays
        .copy_selection()
        .expect("copy-mode survives the resize (ADR-0045: it does not dismiss)");
    assert!(
        fixed.end_row < narrow_pane.h && fixed.end_col < narrow_pane.w,
        "every corner of the selection must be inside the pane the peer's \
             broadcast produced: {fixed:?} vs {narrow_pane:?}",
    );
}

/// phux-foz.3: `resize-pane { direction, amount }` dispatches through
/// `run_action` — the ratio moves by amount/axis-cells, the layout
/// repaints, and the mutation broadcasts via `SET_METADATA` (unlike
/// per-client focus moves).
#[test]
fn resize_pane_dispatch_moves_ratio_and_broadcasts() {
    let mut workspace = two_pane_workspace();
    let before = root_ratio(&workspace);
    let mut action = bare_action("resize-pane");
    action
        .args
        .insert("direction".to_owned(), toml::Value::String("right".into()));
    action
        .args
        .insert("amount".to_owned(), toml::Value::Integer(8));
    let effects = run(&action, &mut workspace);
    assert!(!effects.bell);
    assert!(effects.layout_mutated, "resize repaints the layout");
    assert!(
        effects.set_metadata,
        "a layout mutation broadcasts to other clients"
    );
    let after = root_ratio(&workspace);
    // Growing the focused (left) pane rightward by 8 of 80 cols.
    assert!(
        (after - before - 0.1).abs() < 1e-4,
        "ratio moved {before} -> {after}, wanted +0.1"
    );
}

/// phux-foz.3: a `resize-pane` missing its args bells and mutates
/// nothing (ADR-0019 decision 5 bell-no-op contract).
#[test]
fn resize_pane_dispatch_missing_args_bells() {
    let mut workspace = two_pane_workspace();
    let before = root_ratio(&workspace);
    let effects = run(&bare_action("resize-pane"), &mut workspace);
    assert!(effects.bell);
    assert!(!effects.layout_mutated);
    assert!(!effects.set_metadata);
    assert!((root_ratio(&workspace) - before).abs() < f32::EPSILON);
}

/// phux-foz.3: a resize that would squeeze a pane below the 2-cell
/// floor (ADR-0019 decision 5) bells and leaves the ratio unchanged.
#[test]
fn resize_pane_dispatch_min_cell_floor_bells() {
    let mut workspace = two_pane_workspace();
    let before = root_ratio(&workspace);
    let mut action = bare_action("resize-pane");
    action
        .args
        .insert("direction".to_owned(), toml::Value::String("right".into()));
    action
        .args
        .insert("amount".to_owned(), toml::Value::Integer(80));
    let effects = run(&action, &mut workspace);
    assert!(effects.bell, "floor violation is a bell-no-op");
    assert!(!effects.layout_mutated);
    assert!((root_ratio(&workspace) - before).abs() < f32::EPSILON);
}

/// phux-4h5a: `toggle-sidebar` requests the driver-side flip
/// (`toggle_sidebar`) plus a repaint (`layout_mutated`), unconditionally —
/// even single-pane, since the strip lists windows. It never bells and
/// mutates no tree.
#[test]
fn toggle_sidebar_requests_flip_and_repaint() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("toggle-sidebar"), &mut workspace);
    assert!(effects.toggle_sidebar, "toggle-sidebar requests the flip");
    assert!(
        effects.layout_mutated,
        "sidebar toggle drives a reflow repaint"
    );
    assert!(!effects.bell);
    assert!(!effects.toggle_zoom);
}

/// phux-4h5a: `apply_action_effects` flips the driver-owned
/// `sidebar_enabled` when `toggle_sidebar` is set — off→on and back on a
/// second toggle.
#[allow(
    clippy::too_many_lines,
    reason = "two hand-built DispatchCtx values exercise the full toggle round trip"
)]
#[tokio::test]
async fn apply_effects_flips_sidebar_enabled_state() {
    let mut workspace = Workspace::single(tid(1));
    let mut next_request_id = 1;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut overlays = OverlayState::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut reload_request = false;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    let mut engine_kernel = test_engine_kernel();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: None,
        focus_history: FocusHistory::default(),
        workspace: &mut workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        input_replay: None,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };
    let effects = run_action(
        &bare_action("toggle-sidebar"),
        &mut ctx,
        None,
        &HashMap::new(),
    );
    let mut out: Vec<u8> = Vec::new();
    let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
    let mut conn = Connection::from_stream(a);
    let mut focused_pane = None;
    let mut detach_pending = false;
    let mut predict = PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
    let panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    apply_action_effects(
        effects,
        &mut out,
        &mut conn,
        &mut ctx,
        &mut focused_pane,
        &mut detach_pending,
        &mut predict,
        &panes,
    )
    .await
    .expect("apply effects");
    assert!(sidebar_enabled, "first toggle enables the sidebar");

    // A second toggle disables it again.
    let mut reload_request = false;
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    let mut engine_kernel = test_engine_kernel();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: None,
        focus_history: FocusHistory::default(),
        workspace: &mut workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        input_replay: None,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };
    let effects = run_action(
        &bare_action("toggle-sidebar"),
        &mut ctx,
        None,
        &HashMap::new(),
    );
    apply_action_effects(
        effects,
        &mut out,
        &mut conn,
        &mut ctx,
        &mut focused_pane,
        &mut detach_pending,
        &mut predict,
        &panes,
    )
    .await
    .expect("apply effects");
    assert!(!sidebar_enabled, "second toggle disables the sidebar");
}

#[test]
fn rename_window_with_name_arg_renames_and_broadcasts() {
    let mut workspace = Workspace::single(tid(1)); // window "1"
    let mut action = bare_action("rename-window");
    action
        .args
        .insert("name".to_owned(), toml::Value::String("build".into()));
    let effects = run(&action, &mut workspace);
    assert_eq!(workspace.windows[0].name, "build");
    assert!(effects.layout_mutated);
    assert!(effects.set_metadata, "rename is shared window state");
}

/// Like [`run`], but returns the `OverlayState` so a test can assert
/// an action pushed an overlay.
fn run_capturing(
    action: &phux_config::keybind::ResolvedAction,
    workspace: &mut Workspace,
) -> (ActionEffects, OverlayState) {
    run_capturing_with_sessions(action, workspace, &[], None)
}

/// Like [`run_capturing`], but seeds the dispatcher's cached session
/// graph so `session-picker` tests can drive the picker.
fn run_capturing_with_sessions(
    action: &phux_config::keybind::ResolvedAction,
    workspace: &mut Workspace,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused_session: Option<phux_protocol::ids::SessionId>,
) -> (ActionEffects, OverlayState) {
    let mut next_request_id = 100;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut overlays = OverlayState::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let effects = {
        let mut reload_request = false;
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        // phux-k0cw: the strip's shape comes from the painted target
        // table now, not from the workspace, so a fixture that wants
        // hit-testable window rows must declare them.
        let sidebar_targets = targets(0, workspace.windows.len(), 0);
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            input_replay: None,
            spawn_initial_size_supported: true,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions,
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_width: 20,
            chrome: ChromeBreakpoints::default(),
            sidebar_targets: &sidebar_targets,
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        let focused = ctx.workspace.active_window().and_then(|w| w.focus.clone());
        run_action(action, &mut ctx, focused.as_ref(), &HashMap::new())
    };
    (effects, overlays)
}

#[test]
fn rename_window_no_arg_opens_prompt() {
    let mut workspace = Workspace::single(tid(1)); // window "1"
    let (effects, overlays) = run_capturing(&bare_action("rename-window"), &mut workspace);
    assert!(
        overlays.is_active(),
        "no-arg rename should open the prompt overlay"
    );
    assert!(effects.layout_mutated);
    // Not renamed yet — that happens when the prompt commits.
    assert_eq!(workspace.windows[0].name, "1");
    assert!(!effects.set_metadata, "no broadcast until commit");
}

#[test]
fn kill_window_on_empty_workspace_bells() {
    let mut workspace = Workspace::default();
    let effects = run(&bare_action("kill-window"), &mut workspace);
    assert!(effects.bell);
    assert!(effects.kill_frames.is_empty());
}

#[test]
fn palette_committed_action_routes_through_run_action() {
    // A palette row's ResolvedAction, fed back through run_action,
    // produces the same effect a keybind would. Use `detach` — a row
    // whose effect is unambiguous.
    let cfg = phux_config::parse_str(
        phux_config::DEFAULT_CONFIG_TOML,
        std::path::Path::new("default.toml"),
    )
    .expect("default config parses");
    let items = crate::attach::action_registry::palette_items(Some(&cfg.keybindings), &[], &[]);
    let detach = items
        .iter()
        .find(|i| i.action.action == "detach")
        .expect("detach in palette");
    let mut workspace = Workspace::default();
    let effects = run(&detach.action, &mut workspace);
    assert!(effects.detach, "committing the detach palette row detaches");
}

#[test]
fn plugin_action_records_run_intent_for_the_async_caller() {
    // phux-r82.5: the sync dispatcher never execs the plugin itself —
    // it records (plugin, action) and the async caller spawns the
    // child-process run so the input loop can't freeze on a plugin.
    let mut args = BTreeMap::new();
    args.insert(
        "plugin".to_owned(),
        toml::Value::String("com.example.tools".to_owned()),
    );
    args.insert(
        "action".to_owned(),
        toml::Value::String("summarize".to_owned()),
    );
    let action = phux_config::keybind::ResolvedAction {
        action: "plugin-action".to_owned(),
        args,
    };
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&action, &mut workspace);
    assert_eq!(
        effects.run_plugin,
        Some(("com.example.tools".to_owned(), "summarize".to_owned()))
    );
    assert!(!effects.bell);
    assert!(!effects.layout_mutated, "no repaint for a spawned run");
}

#[test]
fn plugin_action_missing_args_bells() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("plugin-action"), &mut workspace);
    assert!(effects.bell, "missing plugin/action args must bell");
    assert!(effects.run_plugin.is_none());
}

// ---------- phux-r82.7: plugin-pane placement routing ----------

/// Build the `plugin-pane { plugin, pane }` dispatcher action.
fn plugin_pane_action(plugin: &str, pane: &str) -> phux_config::keybind::ResolvedAction {
    let mut args = BTreeMap::new();
    args.insert("plugin".to_owned(), toml::Value::String(plugin.to_owned()));
    args.insert("pane".to_owned(), toml::Value::String(pane.to_owned()));
    phux_config::keybind::ResolvedAction {
        action: "plugin-pane".to_owned(),
        args,
    }
}

/// A hostable pane snapshot entry with the given placement.
fn pane_entry(placement: HostedPlacement) -> PluginPaneEntry {
    PluginPaneEntry {
        plugin_id: "com.example.board".to_owned(),
        plugin_name: "Board".to_owned(),
        pane_id: "board".to_owned(),
        title: "Agent Board".to_owned(),
        placement,
        command: vec!["agent-board".to_owned(), "--watch".to_owned()],
        plugin_root: std::path::PathBuf::from("/plugins/board"),
    }
}

/// Like [`run`], but with a plugin-pane snapshot installed.
fn run_with_panes(
    action: &phux_config::keybind::ResolvedAction,
    workspace: &mut Workspace,
    panes: &[PluginPaneEntry],
) -> ActionEffects {
    let mut next_request_id = 100;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut overlays = OverlayState::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut mouse_optout = std::collections::HashSet::new();
    let mut reload_request = false;
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    let mut engine_kernel = test_engine_kernel();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: None,
        focus_history: FocusHistory::default(),
        workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        input_replay: None,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: panes,
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };
    let focused = ctx.workspace.active_window().and_then(|w| w.focus.clone());
    run_action(action, &mut ctx, focused.as_ref(), &HashMap::new())
}

/// The spawn frame's plugin-relevant fields, destructured for
/// assertions.
struct SpawnParts {
    command: Option<Vec<String>>,
    cwd: Option<String>,
    env: Option<Vec<(String, String)>>,
}

fn spawn_frame_parts(frame: &FrameKind) -> SpawnParts {
    let FrameKind::SpawnTerminal {
        command, cwd, env, ..
    } = frame
    else {
        panic!("expected SpawnTerminal, got {frame:?}");
    };
    SpawnParts {
        command: command.clone(),
        cwd: cwd.clone(),
        env: env.clone(),
    }
}

#[test]
fn plugin_pane_split_placement_parks_pending_split_with_argv_and_env() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run_with_panes(
        &plugin_pane_action("com.example.board", "board"),
        &mut workspace,
        &[pane_entry(HostedPlacement::Split)],
    );
    let (_req, pending, frame) = effects
        .spawn_terminal
        .expect("split placement parks a PendingSplit + SPAWN");
    assert_eq!(pending.focused_at_request, tid(1));
    assert!(!pending.zoom_on_spawn, "plain split must not zoom");
    assert!(effects.spawn_window.is_none());
    let SpawnParts { command, cwd, env } = spawn_frame_parts(&frame);
    assert_eq!(
        command,
        Some(vec!["agent-board".to_owned(), "--watch".to_owned()]),
        "spawn runs the manifest argv, not the default shell",
    );
    assert_eq!(cwd.as_deref(), Some("/plugins/board"));
    let env = env.expect("identity env injected");
    assert!(env.contains(&("PHUX_PLUGIN_ID".to_owned(), "com.example.board".to_owned())));
    assert!(env.contains(&("PHUX_PLUGIN_PANE_ID".to_owned(), "board".to_owned())));
    assert!(env.contains(&("PHUX_PLUGIN_ROOT".to_owned(), "/plugins/board".to_owned())));
}

#[test]
fn plugin_pane_zoomed_placement_requests_zoom_on_spawn() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run_with_panes(
        &plugin_pane_action("com.example.board", "board"),
        &mut workspace,
        &[pane_entry(HostedPlacement::Zoomed)],
    );
    let (_req, pending, _frame) = effects
        .spawn_terminal
        .expect("zoomed placement parks a PendingSplit + SPAWN");
    assert!(pending.zoom_on_spawn, "zoomed placement zooms on reply");
}

#[test]
fn plugin_pane_tab_placement_parks_pending_window_named_after_title() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run_with_panes(
        &plugin_pane_action("com.example.board", "board"),
        &mut workspace,
        &[pane_entry(HostedPlacement::Tab)],
    );
    let (_req, pending, frame) = effects
        .spawn_window
        .expect("tab placement parks a PendingWindow + SPAWN");
    assert_eq!(pending.name, "Agent Board");
    assert!(effects.spawn_terminal.is_none());
    let SpawnParts { command, .. } = spawn_frame_parts(&frame);
    assert_eq!(
        command,
        Some(vec!["agent-board".to_owned(), "--watch".to_owned()])
    );
}

#[test]
fn plugin_pane_unknown_entry_bells() {
    // Covers a disabled plugin, a typo'd id, or an overlay declaration
    // (never snapshotted) reached via a user-config binding.
    let mut workspace = Workspace::single(tid(1));
    let effects = run_with_panes(
        &plugin_pane_action("com.example.absent", "board"),
        &mut workspace,
        &[pane_entry(HostedPlacement::Split)],
    );
    assert!(effects.bell);
    assert!(effects.spawn_terminal.is_none());
    assert!(effects.spawn_window.is_none());
}

#[test]
fn plugin_pane_split_without_focused_pane_bells() {
    let mut workspace = Workspace::default(); // empty: no focus
    let effects = run_with_panes(
        &plugin_pane_action("com.example.board", "board"),
        &mut workspace,
        &[pane_entry(HostedPlacement::Split)],
    );
    assert!(effects.bell);
    assert!(effects.spawn_terminal.is_none());
}

#[test]
fn help_and_command_palette_are_action_finder_aliases() {
    for action in ["show-help", "command-palette"] {
        let mut workspace = Workspace::single(tid(1));
        let (effects, overlays) = run_capturing(&bare_action(action), &mut workspace);
        assert!(
            overlays.is_active(),
            "{action} should push the action finder"
        );
        assert_eq!(overlays.depth(), 1);
        assert!(!effects.layout_mutated);
        assert!(!effects.bell);
    }
}

#[test]
fn getting_started_action_reopens_passthrough_guidance() {
    let mut workspace = Workspace::single(tid(1));
    let (effects, overlays) = run_capturing(&bare_action("getting-started"), &mut workspace);
    assert!(overlays.is_active(), "getting-started should push guidance");
    assert!(
        overlays.top_is_passthrough(),
        "revisited guidance must not consume the dismissing key"
    );
    assert!(!effects.layout_mutated);
    assert!(!effects.bell);
}

#[test]
fn window_picker_action_pushes_overlay_with_windows() {
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("2".to_owned(), tid(2));
    let (effects, overlays) = run_capturing(&bare_action("window-picker"), &mut workspace);
    assert!(overlays.is_active(), "window-picker should push an overlay");
    assert!(!effects.bell);
}

#[test]
fn window_picker_on_empty_workspace_bells() {
    let mut workspace = Workspace::default();
    let (effects, overlays) = run_capturing(&bare_action("window-picker"), &mut workspace);
    assert!(!overlays.is_active(), "no windows ⇒ no overlay");
    assert!(effects.bell);
}

#[test]
fn current_session_window_rows_label_index_name_and_pane_count() {
    let mut workspace = Workspace::single(tid(1)); // window "1", 1 pane
    workspace.add_window("editor".to_owned(), tid(2));
    let items = current_session_window_rows(&workspace);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].label, "0:1");
    assert_eq!(items[0].secondary.as_deref(), Some("1 pane"));
    assert!(items[0].indented, "window rows nest under their session");
    assert_eq!(items[1].label, "1:editor");
    // Each row commits select-window with its index.
    assert_eq!(items[1].action.action, "select-window");
    assert_eq!(
        items[1].action.args.get("index"),
        Some(&toml::Value::Integer(1))
    );
}

#[test]
fn window_picker_groups_windows_under_their_session() {
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("editor".to_owned(), tid(2));
    let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
    let items = window_picker_items(
        &workspace,
        &sessions,
        &HashMap::new(),
        Some(phux_protocol::ids::SessionId::new(1)),
    );
    // Current session ("work") leads, as a header marked "(current)".
    assert!(items[0].is_header());
    assert_eq!(items[0].label, "work (current)");
    // Its windows nest directly beneath, selectable + indented.
    assert!(!items[1].is_header() && items[1].indented);
    assert_eq!(items[1].action.action, "select-window");
    assert_eq!(items[2].action.action, "select-window");
    // The foreign session is a header followed by a switch-session row.
    let scratch = items
        .iter()
        .position(|i| i.is_header() && i.label == "scratch")
        .expect("scratch header present");
    assert_eq!(items[scratch + 1].action.action, "switch-session");
    assert_eq!(
        items[scratch + 1].action.args.get("name"),
        Some(&toml::Value::String("scratch".to_owned())),
    );
    // No cached layout for "scratch" ⇒ no `window` arg (fallback row,
    // plain switch).
    assert!(!items[scratch + 1].action.args.contains_key("window"));
}

/// phux-foz.8: with a peer session's persisted layout cached, the
/// picker lists that session's windows as one-step rows committing
/// `switch-session { name, window }` — same `index:name` + pane-count
/// shape as the current session's rows.
#[test]
fn window_picker_lists_foreign_windows_one_step_when_layout_cached() {
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("editor".to_owned(), tid(2));
    let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
    // scratch's persisted workspace: two windows, "build" and "logs".
    let mut scratch_ws = Workspace::single(tid(10));
    scratch_ws.rename_active("build".to_owned());
    scratch_ws.add_window("logs".to_owned(), tid(11));
    let mut foreign = HashMap::new();
    foreign.insert(phux_protocol::ids::SessionId::new(2), scratch_ws);

    let items = window_picker_items(
        &workspace,
        &sessions,
        &foreign,
        Some(phux_protocol::ids::SessionId::new(1)),
    );
    let scratch = items
        .iter()
        .position(|i| i.is_header() && i.label == "scratch")
        .expect("scratch header present");
    // Two one-step window rows, indented under the header.
    let row0 = &items[scratch + 1];
    let row1 = &items[scratch + 2];
    assert_eq!(row0.label, "0:build");
    assert_eq!(row0.secondary.as_deref(), Some("1 pane"));
    assert!(row0.indented);
    assert_eq!(row0.action.action, "switch-session");
    assert_eq!(
        row0.action.args.get("name"),
        Some(&toml::Value::String("scratch".to_owned())),
    );
    assert_eq!(
        row0.action.args.get("window"),
        Some(&toml::Value::Integer(0)),
    );
    assert_eq!(row1.label, "1:logs");
    assert_eq!(
        row1.action.args.get("window"),
        Some(&toml::Value::Integer(1)),
    );
    // No fallback "switch to this session" row when windows list.
    assert!(
        items.iter().all(|i| i.label != "switch to this session"),
        "one-step rows replace the fallback row"
    );
}

/// phux-foz.8: an empty cached workspace (decoded but windowless) is
/// not useful — the picker falls back to the plain switch row.
#[test]
fn window_picker_empty_foreign_layout_falls_back_to_switch_row() {
    let workspace = Workspace::single(tid(1));
    let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
    let mut foreign = HashMap::new();
    foreign.insert(phux_protocol::ids::SessionId::new(2), Workspace::default());
    let items = window_picker_items(
        &workspace,
        &sessions,
        &foreign,
        Some(phux_protocol::ids::SessionId::new(1)),
    );
    let scratch = items
        .iter()
        .position(|i| i.is_header() && i.label == "scratch")
        .expect("scratch header present");
    assert_eq!(items[scratch + 1].label, "switch to this session");
    assert!(!items[scratch + 1].action.args.contains_key("window"));
}

/// phux-foz.8: committing a one-step picker row through `run_action`
/// yields the combined reattach target — session name AND window index
/// — that the driver resolves after the re-attach.
#[test]
fn one_step_picker_row_commits_switch_session_with_window() {
    let mut workspace = Workspace::single(tid(1));
    let mut scratch_ws = Workspace::single(tid(10));
    scratch_ws.add_window("logs".to_owned(), tid(11));
    let rows = foreign_session_window_rows("scratch", &scratch_ws);
    assert_eq!(rows.len(), 2);
    let effects = run(&rows[1].action, &mut workspace);
    assert_eq!(
        effects.reattach,
        Some(ReattachTarget::Existing {
            name: "scratch".to_owned(),
            window: Some(1),
            pane: None,
        }),
        "the one-step row carries the target window through dispatch"
    );
    // The switch is a re-attach, not a local window change.
    assert_eq!(workspace.active, 0);
}

/// phux-foz.8: a `switch-session` with a bad `window` arg (negative /
/// non-integer) degrades to a plain switch rather than belling — the
/// `name` is still valid and honoring it is strictly more useful.
#[test]
fn switch_session_bad_window_arg_degrades_to_plain_switch() {
    let mut workspace = Workspace::single(tid(1));
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), toml::Value::String("scratch".to_owned()));
    args.insert("window".to_owned(), toml::Value::Integer(-3));
    let action = phux_config::keybind::ResolvedAction {
        action: "switch-session".to_owned(),
        args,
    };
    let effects = run(&action, &mut workspace);
    assert_eq!(
        effects.reattach,
        Some(ReattachTarget::Existing {
            name: "scratch".to_owned(),
            window: None,
            pane: None,
        }),
    );
    assert!(!effects.bell);
}

/// phux-jpqd: a `switch-session { name, window, pane }` — the commit the
/// agent-fleet dashboard's foreign pane rows carry — parses into the
/// combined one-step cross-session pane target.
#[test]
fn switch_session_with_pane_arg_carries_one_step_pane_target() {
    let mut workspace = Workspace::single(tid(1));
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), toml::Value::String("scratch".to_owned()));
    args.insert("window".to_owned(), toml::Value::Integer(1));
    args.insert("pane".to_owned(), toml::Value::Integer(2));
    let action = phux_config::keybind::ResolvedAction {
        action: "switch-session".to_owned(),
        args,
    };
    let effects = run(&action, &mut workspace);
    assert_eq!(
        effects.reattach,
        Some(ReattachTarget::Existing {
            name: "scratch".to_owned(),
            window: Some(1),
            pane: Some(2),
        }),
    );
    assert!(!effects.bell);
    // The switch is a re-attach, not a local change.
    assert_eq!(workspace.active, 0);
}

#[test]
fn window_picker_commit_routes_select_window_through_run_action() {
    // The architectural invariant: a picker selection commits a
    // select-window ResolvedAction that, when fed back through
    // run_action, performs the same per-client switch a numeric prefix
    // binding does.
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("2".to_owned(), tid(2));
    workspace.select(0); // active = 0
    let items = current_session_window_rows(&workspace);
    // Commit the picker row for window index 1.
    let effects = run(&items[1].action, &mut workspace);
    assert_eq!(
        workspace.active, 1,
        "select-window switched the active window"
    );
    assert!(effects.layout_mutated);
    assert_eq!(effects.set_focus, Some(tid(2)));
    assert!(!effects.set_metadata, "window switch is per-client");
}

// ---------- phux-oih5.16: client-local attention navigation ----------

/// Run an attention action with caller-owned pane flags and excursion
/// state, so successive dispatches exercise origin preservation.
fn run_attention(
    action: &str,
    workspace: &mut Workspace,
    panes: &HashMap<TerminalId, PaneSlot>,
    navigation: &mut AttentionNavigation,
) -> ActionEffects {
    let mut next_request_id = 100;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut overlays = OverlayState::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag = None;
    let mut reload_request = false;
    let mut mouse_optout = std::collections::HashSet::new();
    let agent_meta = HashMap::new();
    let mut vcs = crate::attach::pane_state::VcsIndex::default();
    let focus_history = FocusHistory::default();
    let focused = workspace.active_window().and_then(|w| w.focus.clone());
    let mut engine_kernel = test_engine_kernel();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: None,
        workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        input_replay: None,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: navigation,
        focus_history,
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &agent_meta,
        vcs: &mut vcs,
    };
    run_action(&bare_action(action), &mut ctx, focused.as_ref(), panes)
}

fn asking_panes(ids: &[u32]) -> HashMap<TerminalId, PaneSlot> {
    ids.iter()
        .map(|id| {
            let terminal = tid(*id);
            let mut slot = PaneSlot::new_with_size(20, 4).expect("pane slot");
            slot.attention = true;
            (terminal, slot)
        })
        .collect()
}

#[test]
fn next_attention_with_no_attention_is_a_local_bell_noop() {
    let mut workspace = fleet_workspace();
    workspace.select(0);
    let before = workspace.clone();
    let mut navigation = AttentionNavigation::default();
    let effects = run_attention(
        "next-attention",
        &mut workspace,
        &HashMap::new(),
        &mut navigation,
    );
    assert!(effects.bell);
    assert!(!effects.layout_mutated);
    assert!(!effects.set_metadata);
    assert_eq!(workspace, before);
    assert!(
        navigation.take_origin().is_none(),
        "no jump must not save an origin"
    );
}

#[test]
fn next_attention_cycles_window_then_dfs_with_wrap_and_one_origin() {
    // fleet_workspace is window 0 DFS [1,2], then window 1 DFS [3].
    let mut workspace = fleet_workspace();
    workspace.select(0);
    let panes = asking_panes(&[2, 3]);
    let mut navigation = AttentionNavigation::default();

    let first = run_attention("next-attention", &mut workspace, &panes, &mut navigation);
    assert_eq!(workspace.active, 0);
    assert_eq!(workspace.windows[0].state.focus, Some(tid(2)));
    assert_eq!(first.set_focus, Some(tid(2)));
    assert!(!first.set_metadata, "attention focus is never shared");

    let cross = run_attention("next-attention", &mut workspace, &panes, &mut navigation);
    assert_eq!(
        workspace.active, 1,
        "cycle crosses windows in display order"
    );
    assert_eq!(cross.set_focus, Some(tid(3)));
    assert!(!cross.set_metadata);

    let wrapped = run_attention("next-attention", &mut workspace, &panes, &mut navigation);
    assert_eq!(workspace.active, 0, "last asking pane wraps to the first");
    assert_eq!(wrapped.set_focus, Some(tid(2)));
    assert!(!wrapped.set_metadata);

    let returned = run_attention(
        "return-from-attention",
        &mut workspace,
        &panes,
        &mut navigation,
    );
    assert_eq!(
        returned.set_focus,
        Some(tid(1)),
        "cycling kept the first origin"
    );
    assert_eq!(workspace.windows[0].state.focus, Some(tid(1)));
    assert!(!returned.set_metadata);

    let consumed = run_attention(
        "return-from-attention",
        &mut workspace,
        &panes,
        &mut navigation,
    );
    assert!(consumed.bell, "return consumes the single saved origin");
    assert!(!consumed.layout_mutated);
    assert!(!consumed.set_metadata);
}

#[test]
fn return_from_attention_consumes_a_stale_origin_safely() {
    let mut workspace = fleet_workspace();
    workspace.select(0);
    let panes = asking_panes(&[2]);
    let mut navigation = AttentionNavigation::default();
    let jumped = run_attention("next-attention", &mut workspace, &panes, &mut navigation);
    assert_eq!(jumped.set_focus, Some(tid(2)));

    // The original pane closes while the user is examining the question.
    workspace.windows[0].state = crate::layout::LayoutState::single(tid(2));
    let before = workspace.clone();
    let stale = run_attention(
        "return-from-attention",
        &mut workspace,
        &panes,
        &mut navigation,
    );
    assert!(stale.bell);
    assert!(!stale.layout_mutated);
    assert!(stale.set_focus.is_none());
    assert!(!stale.set_metadata);
    assert_eq!(
        workspace, before,
        "stale return must not focus another pane"
    );

    let consumed = run_attention(
        "return-from-attention",
        &mut workspace,
        &panes,
        &mut navigation,
    );
    assert!(
        consumed.bell,
        "stale origin is consumed on the first return"
    );
    assert!(!consumed.layout_mutated);
}

// ---------- phux-foz.7: agent-fleet dashboard + focus-pane ----------

#[test]
fn agent_fleet_action_pushes_overlay() {
    let mut workspace = Workspace::single(tid(1));
    let (effects, overlays) = run_capturing(&bare_action("agent-fleet"), &mut workspace);
    assert!(overlays.is_active(), "agent-fleet should push the overlay");
    assert_eq!(overlays.depth(), 1);
    assert!(!effects.bell);
}

#[test]
fn agent_fleet_on_empty_workspace_bells() {
    let mut workspace = Workspace::default();
    let (effects, overlays) = run_capturing(&bare_action("agent-fleet"), &mut workspace);
    assert!(!overlays.is_active(), "nothing to list => no overlay");
    assert!(effects.bell);
}

#[test]
fn agent_fleet_overlay_accepts_live_fleet_refresh() {
    // The pushed overlay is constructed with the fleet live key, so the
    // driver's push-based refresh (rows rebuilt when an agent event
    // lands) reaches it in place.
    let mut workspace = Workspace::single(tid(1));
    let (_effects, mut overlays) = run_capturing(&bare_action("agent-fleet"), &mut workspace);
    let fresh = crate::attach::fleet::fleet_items(
        &workspace,
        &[],
        None,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(
        overlays.refresh_items(crate::attach::fleet::FLEET_LIVE_KEY, &fresh),
        "the fleet overlay must accept a matching live refresh"
    );
}

#[test]
fn command_palette_ignores_fleet_refresh() {
    // Static overlays (the palette, the pickers) must never swap their
    // rows for fleet data.
    let mut workspace = Workspace::single(tid(1));
    let (_effects, mut overlays) = run_capturing(&bare_action("command-palette"), &mut workspace);
    assert!(
        !overlays.refresh_items(crate::attach::fleet::FLEET_LIVE_KEY, &[]),
        "a static overlay must ignore the fleet refresh"
    );
}

/// Window 0 split into panes 1|2, window 1 a single pane 3.
fn fleet_workspace() -> Workspace {
    use crate::layout::{LayoutNode, LayoutState, SplitDir, WindowState, split_at};
    let tree = split_at(
        &LayoutNode::Leaf(tid(1)),
        &tid(1),
        &tid(2),
        SplitDir::Horizontal,
        0.5,
    )
    .unwrap();
    Workspace {
        windows: vec![
            WindowState {
                name: "main".to_owned(),
                state: LayoutState {
                    tree: Some(tree),
                    focus: Some(tid(1)),
                },
            },
            WindowState {
                name: "logs".to_owned(),
                state: LayoutState::single(tid(3)),
            },
        ],
        active: 1,
    }
}

#[test]
fn focus_pane_switches_window_and_focuses_leaf() {
    let mut workspace = fleet_workspace(); // active = window 1
    let mut action = bare_action("focus-pane");
    action
        .args
        .insert("window".to_owned(), toml::Value::Integer(0));
    action
        .args
        .insert("pane".to_owned(), toml::Value::Integer(1));
    let effects = run(&action, &mut workspace);
    assert_eq!(workspace.active, 0, "switched to the target window");
    assert_eq!(
        workspace.windows[0].state.focus,
        Some(tid(2)),
        "focus landed on the second DFS leaf"
    );
    assert_eq!(effects.set_focus, Some(tid(2)));
    assert!(effects.layout_mutated);
    assert!(!effects.set_metadata, "focus is per-client, no broadcast");
    assert!(!effects.bell);
}

#[test]
fn focus_pane_within_active_window_moves_focus_only() {
    let mut workspace = fleet_workspace();
    workspace.select(0); // active = 0, focus = tid(1)
    let mut action = bare_action("focus-pane");
    action
        .args
        .insert("window".to_owned(), toml::Value::Integer(0));
    action
        .args
        .insert("pane".to_owned(), toml::Value::Integer(1));
    let effects = run(&action, &mut workspace);
    assert_eq!(workspace.active, 0);
    assert_eq!(workspace.windows[0].state.focus, Some(tid(2)));
    assert_eq!(effects.set_focus, Some(tid(2)));
}

#[test]
fn focus_pane_missing_args_bells() {
    let mut workspace = fleet_workspace();
    let effects = run(&bare_action("focus-pane"), &mut workspace);
    assert!(effects.bell);
    assert!(effects.set_focus.is_none());
}

#[test]
fn focus_pane_stale_coordinates_bell_without_mutation() {
    // The fleet rows may outlive a layout change; a stale (window, pane)
    // address must bell rather than focus the wrong pane.
    let mut workspace = fleet_workspace();
    let mut action = bare_action("focus-pane");
    action
        .args
        .insert("window".to_owned(), toml::Value::Integer(0));
    action
        .args
        .insert("pane".to_owned(), toml::Value::Integer(9));
    let effects = run(&action, &mut workspace);
    assert!(effects.bell);
    assert_eq!(workspace.active, 1, "no window switch on a stale address");
    assert!(effects.set_focus.is_none());
}

#[test]
fn fleet_commit_routes_focus_pane_through_run_action() {
    // The architectural invariant: a fleet row's committed ResolvedAction,
    // fed back through run_action, performs the same per-client focus a
    // keybinding path would.
    let mut workspace = fleet_workspace();
    let items = crate::attach::fleet::fleet_items(
        &workspace,
        &[],
        None,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    // Row 1 is window 0's second pane (tid 2).
    let effects = run(&items[1].action.clone(), &mut workspace);
    assert_eq!(workspace.active, 0);
    assert_eq!(effects.set_focus, Some(tid(2)));
}

fn sinfo(id: u32, name: &str) -> phux_protocol::wire::info::SessionInfo {
    phux_protocol::wire::info::SessionInfo::new(phux_protocol::ids::SessionId::new(id), name)
        .with_window_count(1)
}

#[test]
fn session_picker_items_include_focused_first_and_commit_switch_session() {
    let sessions = [sinfo(1, "work"), sinfo(2, "scratch"), sinfo(3, "logs")];
    let items = session_picker_items(&sessions, Some(phux_protocol::ids::SessionId::new(1)));
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].label, "work");
    assert_eq!(items[0].secondary.as_deref(), Some("1 window, current"));
    assert_eq!(items[1].label, "logs");
    assert_eq!(items[2].label, "scratch");
    // Each row commits switch-session with the session name.
    assert_eq!(items[0].action.action, "switch-session");
    assert_eq!(
        items[0].action.args.get("name"),
        Some(&toml::Value::String("work".to_owned()))
    );
    assert_eq!(items[1].secondary.as_deref(), Some("1 window"));
}

#[test]
fn session_picker_action_pushes_overlay_with_peer_sessions() {
    let mut workspace = Workspace::single(tid(1));
    let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
    let (effects, overlays) = run_capturing_with_sessions(
        &bare_action("session-picker"),
        &mut workspace,
        &sessions,
        Some(phux_protocol::ids::SessionId::new(1)),
    );
    assert!(
        overlays.is_active(),
        "session-picker should push an overlay"
    );
    assert!(!effects.bell);
}

#[test]
fn session_picker_with_only_current_session_still_opens_for_new() {
    // Even when the client's own session is the only one, the picker
    // opens so the user can create a new session via the "+ New
    // session" row — it no longer bells into a dead end.
    let mut workspace = Workspace::single(tid(1));
    let sessions = [sinfo(1, "work")];
    let (effects, overlays) = run_capturing_with_sessions(
        &bare_action("session-picker"),
        &mut workspace,
        &sessions,
        Some(phux_protocol::ids::SessionId::new(1)),
    );
    assert!(overlays.is_active(), "picker opens to offer + New session");
    assert!(!effects.bell);
}

#[test]
fn session_picker_with_no_sessions_still_opens_for_new() {
    // Before the first ATTACHED snapshot lands the cache is empty; the
    // picker still opens with the "+ New session" row.
    let mut workspace = Workspace::single(tid(1));
    let (effects, overlays) =
        run_capturing_with_sessions(&bare_action("session-picker"), &mut workspace, &[], None);
    assert!(overlays.is_active());
    assert!(!effects.bell);
}

#[test]
fn session_picker_commit_routes_switch_session_through_run_action() {
    // The architectural invariant: a picker row commits a
    // switch-session ResolvedAction that, fed back through run_action,
    // yields the reattach effect keyed by the chosen name.
    let mut workspace = Workspace::single(tid(1));
    let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
    let items = session_picker_items(&sessions, Some(phux_protocol::ids::SessionId::new(1)));
    let scratch = items
        .iter()
        .find(|item| item.label == "scratch")
        .expect("peer session row");
    let effects = run(&scratch.action, &mut workspace);
    assert_eq!(
        effects.reattach,
        Some(ReattachTarget::Existing {
            name: "scratch".to_owned(),
            window: None,
            pane: None,
        }),
        "committing the picker row requests a switch to that session"
    );
}

#[test]
fn switch_session_missing_name_bells() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("switch-session"), &mut workspace);
    assert!(effects.reattach.is_none());
    assert!(effects.bell, "a switch-session with no name arg bells");
}

#[test]
fn new_session_with_name_requests_create_reattach() {
    let mut workspace = Workspace::single(tid(1));
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), toml::Value::String("scratch".to_owned()));
    let action = phux_config::keybind::ResolvedAction {
        action: "new-session".to_owned(),
        args,
    };
    let effects = run(&action, &mut workspace);
    assert_eq!(
        effects.reattach,
        Some(ReattachTarget::Create("scratch".to_owned())),
        "new-session with a name requests a create-and-switch"
    );
}

#[test]
fn new_session_without_name_opens_prompt() {
    let mut workspace = Workspace::single(tid(1));
    let (effects, overlays) = run_capturing(&bare_action("new-session"), &mut workspace);
    assert!(
        overlays.is_active(),
        "new-session with no name opens the name prompt"
    );
    assert!(
        effects.reattach.is_none(),
        "the prompt commit drives the re-attach later"
    );
}

#[test]
fn detach_action_requests_detach_effect() {
    let mut workspace = Workspace::default();
    let mut next_request_id = 1;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut overlays = OverlayState::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut reload_request = false;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    let mut engine_kernel = test_engine_kernel();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: None,
        focus_history: FocusHistory::default(),
        workspace: &mut workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        input_replay: None,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };
    let action = phux_config::keybind::ResolvedAction {
        action: "detach".to_owned(),
        args: BTreeMap::new(),
    };

    let effects = run_action(&action, &mut ctx, None, &HashMap::new());

    assert!(effects.detach);
    assert!(!effects.layout_mutated);
}

#[test]
fn rename_session_with_name_arg_requests_rename_effect() {
    // An explicit `name` produces the rename-session effect carrying the
    // new name; no prompt is opened. The send + local-name update happen
    // in `apply_action_effects` (async), so run_action only sets the
    // effect.
    let mut workspace = Workspace::single(tid(1));
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), toml::Value::String("notes".to_owned()));
    let action = phux_config::keybind::ResolvedAction {
        action: "rename-session".to_owned(),
        args,
    };
    let effects = run(&action, &mut workspace);
    assert_eq!(
        effects.rename_session.as_deref(),
        Some("notes"),
        "rename-session with a name requests the rename effect",
    );
}

#[test]
fn rename_session_without_name_opens_prompt_prefilled() {
    // No `name` arg opens the prompt pre-filled with the current session
    // name; the rename itself is deferred to the prompt commit.
    let mut workspace = Workspace::single(tid(1));
    let mut next_request_id = 100;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut overlays = OverlayState::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = "work".to_owned();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let effects = {
        let mut reload_request = false;
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        // phux-k0cw: the strip's shape comes from the painted target
        // table now, not from the workspace, so a fixture that wants
        // hit-testable window rows must declare them.
        let sidebar_targets = targets(0, workspace.windows.len(), 0);
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            input_replay: None,
            spawn_initial_size_supported: true,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_width: 20,
            chrome: ChromeBreakpoints::default(),
            sidebar_targets: &sidebar_targets,
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        run_action(
            &bare_action("rename-session"),
            &mut ctx,
            None,
            &HashMap::new(),
        )
    };
    assert!(
        overlays.is_active(),
        "no-arg rename-session opens the name prompt",
    );
    assert!(
        effects.rename_session.is_none(),
        "the prompt commit drives the rename later",
    );
}

#[test]
fn rename_session_prompt_commits_rename_session_action() {
    // The prompt the bare action opens must commit a
    // `rename-session { name }` ResolvedAction, so feeding it back
    // through run_action yields the rename effect (the same single
    // dispatch path rename-window uses).
    use crate::render::overlay::{OverlayCommand, RenderOverlay};
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

    let mut prompt = PromptOverlay::rename_session("work", &Theme::default());
    let press = |key: PhysicalKey, text: Option<&str>| KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(ToOwned::to_owned),
        unshifted_codepoint: None,
    };
    // Clear the prefilled "work" and type "notes".
    for _ in 0..4 {
        let _ = prompt.handle_key(&press(PhysicalKey::Backspace, None));
    }
    for ch in ['n', 'o', 't', 'e', 's'] {
        let _ = prompt.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
    }
    let OverlayCommand::Commit(resolved) = prompt.handle_key(&press(PhysicalKey::Enter, None))
    else {
        panic!("Enter on a non-empty prompt should commit");
    };
    assert_eq!(resolved.action, "rename-session");

    let mut workspace = Workspace::single(tid(1));
    let effects = run(&resolved, &mut workspace);
    assert_eq!(
        effects.rename_session.as_deref(),
        Some("notes"),
        "the committed prompt action yields the rename effect with the typed name",
    );
}
