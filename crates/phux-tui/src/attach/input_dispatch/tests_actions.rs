//! Action-dispatch tests: `run_action` arms, pickers, attention
//! navigation, and session flows.
#![allow(clippy::expect_used, reason = "tests")]

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
use phux_protocol::ids::{SatelliteHost, SessionId};
use phux_protocol::wire::frame::{Command, FrameKind};
use phux_protocol::wire::info::{HostInventory, HostSessionInfo, SessionInfo};
use toml::Value;

use crate::attach::actions::SplitHost;
use crate::attach::directory_picker::{DirectorySupport, ListingHost};
use crate::attach::focus::FocusHistory;
use crate::attach::pane_state::PaneSlot;
use crate::attach::plugin_panes::{HostedPlacement, PluginPaneEntry};
use crate::layout::{LayoutNode, LayoutState, SplitDir, WindowState, Workspace, split_at};
use crate::render::Theme;
use crate::render::overlay::{CopyModeOverlay, OverlayState, PromptOverlay};

use super::args::*;
use super::dispatch::*;
use super::effects::*;
use super::pickers::*;
use super::test_support::*;

/// Run `action` against `workspace` on a current server.
fn run(action: &phux_config::keybind::ResolvedAction, workspace: &mut Workspace) -> ActionEffects {
    let mut f = fx(std::mem::take(workspace));
    let effects = f.run(action);
    *workspace = f.workspace;
    effects
}

fn edge() -> SatelliteHost {
    SatelliteHost::new("edge")
}

fn satellite_id(host: &str, id: u32) -> ResourceId {
    ResourceId::satellite(SatelliteHost::new(host), id)
}

fn sinfo(id: u32, name: &str) -> SessionInfo {
    SessionInfo::new(SessionId::new(id), name).with_window_count(1)
}

fn names(workspace: &Workspace) -> Vec<&str> {
    workspace.windows.iter().map(|w| w.name.as_str()).collect()
}

fn three_windows() -> Workspace {
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("2".to_owned(), tid(2));
    workspace.add_window("3".to_owned(), tid(3)); // active = 2
    workspace
}

/// Window 0 split into panes 1|2, window 1 a single pane 3 (active).
fn fleet_workspace() -> Workspace {
    let mut workspace = two_pane_workspace();
    workspace.windows[0].name = "main".to_owned();
    workspace.windows.push(WindowState::new(
        "logs".to_owned(),
        LayoutState::single(tid(3)),
    ));
    workspace.active = 1;
    workspace
}

fn spawn_initial_size_of(frame: &FrameKind) -> Option<(u16, u16)> {
    let FrameKind::SpawnResource { initial_size, .. } = frame else {
        panic!("expected SpawnResource, got {frame:?}");
    };
    *initial_size
}

/// Whether a spawn asks for the instance binding (ADR-0109).
fn asks_binding(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::SpawnResource { resource: Some(resource), .. } if resource.bind_instance
    )
}

/// `split-pane` with `direction = vertical`.
fn split_action() -> phux_config::keybind::ResolvedAction {
    act("split-pane", &[("direction", "vertical".into())])
}

/// A focused satellite pane with `cwd`, as a fixture plus its pane slots.
fn satellite_pane(cwd: Option<&str>) -> (CtxFixture, HashMap<ResourceId, PaneSlot>) {
    let pane = satellite_id("edge", 9);
    let mut slot = PaneSlot::new().expect("pane slot");
    slot.cwd = cwd.map(str::to_owned);
    (
        fx(Workspace::single(pane.clone())),
        HashMap::from([(pane, slot)]),
    )
}

/// A hub that predates host-aware listing and spawns.
fn older_hub(f: &mut CtxFixture) {
    f.directory_support = DirectorySupport::from_features(ServerFeatureSet::with(&[
        ServerFeature::SpawnInitialSize,
        ServerFeature::ListDirectory,
    ]));
}

// ---- small helpers and frames ----------------------------------------------

/// Regression: kill-pane once typed `exit\n` at the pane and hoped a shell
/// was listening. It is one correlated `KILL_RESOURCE` (a `TERMINAL_NOT_FOUND`
/// refusal is the only evidence a stale leaf should leave the layout).
#[test]
fn kill_resource_frame_targets_the_pane_with_a_correlated_command() {
    assert_eq!(
        kill_resource_frame(&tid(7), 42),
        FrameKind::Command {
            request_id: 42,
            command: Command::KillResource {
                terminal_id: tid(7),
                operation_id: None,
            },
        }
    );
}

/// `direction` names the divider orientation, not the split axis.
#[test]
fn split_dir_arg_parses_horizontal_and_vertical() {
    for (value, dir) in [
        ("horizontal", Some(SplitDir::Vertical)),
        ("vertical", Some(SplitDir::Horizontal)),
        ("diagonal", None),
    ] {
        assert_eq!(
            split_dir_arg(&act("split-pane", &[("direction", value.into())])),
            dir
        );
    }
}

#[test]
fn focused_pane_rect_tracks_rendered_pane_bounds() {
    use crate::layout::Rect;
    use crate::render::chrome::status_bar::Position;
    let mut workspace = two_pane_workspace();
    workspace.windows[0].state.focus = Some(tid(2));
    let bottom = Some(Position::Bottom);
    let split = focused_pane_rect_for(&workspace, None, Some(&tid(2)), (80, 24), bottom, None);
    // Row 0 is the pane rail; the bar row is not copy-mode content either.
    assert_eq!((split.y, split.h, split.x + split.w), (1, 22, 80));
    assert!(
        split.w < 80,
        "a split pane must not inherit the viewport width"
    );
    // A zoomed pane takes the whole content rect, rail excluded.
    let zoomed = focused_pane_rect_for(
        &workspace,
        Some(&tid(2)),
        Some(&tid(2)),
        (80, 24),
        bottom,
        None,
    );
    assert_eq!(
        zoomed,
        Rect {
            x: 0,
            y: 1,
            w: 80,
            h: 22
        }
    );
}

/// A peer's layout broadcast can shrink the focused pane with no SIGWINCH.
/// Copy mode must adopt the new rect, or its stranded corner resolves to
/// nothing and Enter silently copies nothing.
#[test]
fn layout_replace_reclaims_the_focused_pane_rect_for_overlays() {
    let viewport = (80, 24);
    let wide = focused_pane_rect_for(
        &two_pane_workspace(),
        None,
        Some(&tid(1)),
        viewport,
        None,
        None,
    );
    let mut overlays = OverlayState::new();
    overlays.push(Box::new(CopyModeOverlay::new(
        wide.h.saturating_sub(1),
        wide.w.saturating_sub(1),
        wide.w,
        wide.h,
    )));
    let narrow_ws = two_pane_workspace_at(0.1);
    let narrow = focused_pane_rect_for(&narrow_ws, None, Some(&tid(1)), viewport, None, None);
    let stale = overlays.copy_selection().expect("selection survives");
    assert!(
        stale.end_col >= narrow.w,
        "precondition: corner stranded outside"
    );

    sync_overlays_to_focused_pane(
        &mut overlays,
        &narrow_ws,
        None,
        Some(&tid(1)),
        viewport,
        None,
        None,
    );
    let fixed = overlays
        .copy_selection()
        .expect("copy-mode survives the resize");
    assert!(
        fixed.end_row < narrow.h && fixed.end_col < narrow.w,
        "{fixed:?} vs {narrow:?}"
    );
}

// ---- windows, panes, and spawns --------------------------------------------

#[test]
fn reload_config_action_raises_only_the_reload_effect() {
    let effects = run(
        &bare_action("reload-config"),
        &mut Workspace::single(tid(1)),
    );
    assert!(effects.reload_config);
    assert!(!effects.layout_mutated && !effects.bell && effects.kill_frames.is_empty());
}

#[test]
fn new_window_parks_pending_and_emits_a_sized_unbound_spawn() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("new-window"), &mut workspace);
    let (_, pending, frame) = effects.spawn_window.expect("new-window parks a SPAWN");
    assert_eq!(pending.name, "2", "the default name skips the in-use \"1\"");
    // One leaf fills the 80x23 content rect (row 0 is the rail).
    assert_eq!(spawn_initial_size_of(&frame), Some((80, 23)));
    assert!(
        matches!(&frame, FrameKind::SpawnResource { resource: None, .. }),
        "{frame:?}"
    );
    assert_eq!(workspace.windows.len(), 1, "the window opens on reply");

    // ADR-0105: new-window also works from a keep-empty session's empty state.
    let effects = run(&bare_action("new-window"), &mut Workspace::default());
    let (_, _, frame) = effects.spawn_window.expect("spawns from the empty state");
    assert!(matches!(
        frame,
        FrameKind::SpawnResource {
            owner_terminal: None,
            ..
        }
    ));
}

/// `new-window { cwd, host }` (the directory picker's confirm row) spawns in
/// that directory, on that satellite when a host is named.
#[test]
fn new_window_cwd_and_host_args_ride_the_spawn() {
    let cwd = act("new-window", &[("cwd", "/srv/app".into())]);
    let (_, _, frame) = run(&cwd, &mut Workspace::single(tid(1)))
        .spawn_window
        .expect("SPAWN");
    assert!(
        matches!(&frame, FrameKind::SpawnResource { cwd: Some(c), satellite: None, .. } if c == "/srv/app"),
        "{frame:?}"
    );

    let hosted = act(
        "new-window",
        &[("cwd", "/home/e/src".into()), ("host", "edge".into())],
    );
    let (_, _, frame) = run(&hosted, &mut Workspace::single(tid(1)))
        .spawn_window
        .expect("SPAWN");
    assert!(
        matches!(&frame, FrameKind::SpawnResource { cwd: Some(c), satellite: Some(h), .. } if c == "/home/e/src" && *h == edge()),
        "{frame:?}"
    );
    assert!(asks_binding(&frame));
}

/// A split names the tile its new leaf will occupy (80x23 split with one
/// divider column: 40/39), so the server bootstraps it at the right size.
#[test]
fn split_pane_spawn_carries_the_new_leafs_tile() {
    let effects = run(&split_action(), &mut Workspace::single(tid(1)));
    let (_, _, frame) = effects.spawn_terminal.expect("split parks a SPAWN");
    assert_eq!(spawn_initial_size_of(&frame), Some((39, 23)));
}

/// Without `ServerFeature::SpawnInitialSize` the field stays absent
/// (ADR-0061: no dependence on unadvertised surface).
#[test]
fn spawn_omits_initial_size_when_the_server_did_not_advertise_it() {
    for action in [split_action(), bare_action("new-window")] {
        let mut f = fx(Workspace::single(tid(1)));
        f.spawn_initial_size_supported = false;
        let effects = f.run(&action);
        let frame = effects
            .spawn_terminal
            .map(|(_, _, frame)| frame)
            .or_else(|| effects.spawn_window.map(|(_, _, frame)| frame))
            .expect("SPAWN");
        assert_eq!(spawn_initial_size_of(&frame), None, "{}", action.action);
    }
}

/// kill-window kills every leaf (each correlated, each an expected close);
/// kill-pane its focused one. Nothing is removed until the closes land.
#[test]
fn kill_window_and_kill_pane_emit_correlated_expected_kills() {
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
        windows: vec![WindowState::new(
            "1".to_owned(),
            LayoutState {
                tree: Some(tree),
                focus: Some(tid(1)),
            },
        )],
        active: 0,
    };
    for (action, killed) in [
        ("kill-window", vec![tid(1), tid(2), tid(3)]),
        ("kill-pane", vec![tid(1)]),
    ] {
        let effects = run(&bare_action(action), &mut workspace);
        assert_eq!(effects.kill_frames.len(), killed.len(), "{action}");
        let targets: Vec<_> = effects
            .kill_requests
            .iter()
            .map(|(_, leaf)| leaf.clone())
            .collect();
        assert_eq!(targets, killed, "{action}");
        assert_eq!(effects.expected_closes, killed, "{action}");
        assert_eq!(workspace.windows.len(), 1);
    }
    let effects = run(&bare_action("kill-window"), &mut Workspace::default());
    assert!(effects.bell && effects.kill_frames.is_empty());
}

#[test]
fn window_navigation_switches_active_without_broadcasting() {
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("2".to_owned(), tid(2));
    workspace.select(0);
    let effects = run(&bare_action("next-window"), &mut workspace);
    assert_eq!(workspace.active, 1);
    assert!(effects.layout_mutated && effects.clear_predict);
    assert!(!effects.set_metadata, "window switch is per-client");
    assert_eq!(effects.set_focus, Some(tid(2)));

    let mut single = Workspace::single(tid(1));
    let effects = run(&bare_action("next-window"), &mut single);
    assert!(!effects.layout_mutated && !effects.clear_predict);

    let mut workspace = three_windows();
    let effects = run(
        &act("select-window", &[("index", 0.into())]),
        &mut workspace,
    );
    assert_eq!(workspace.active, 0);
    assert!(effects.layout_mutated);
    assert_eq!(effects.set_focus, Some(tid(1)));
    let effects = run(
        &act("select-window", &[("index", 5.into())]),
        &mut workspace,
    );
    assert!(!effects.layout_mutated, "out of range is a no-op");
    let effects = run(&bare_action("select-window"), &mut workspace);
    assert!(
        effects.bell && !effects.layout_mutated,
        "missing index bells"
    );
}

#[test]
fn last_pane_jumps_across_windows_and_toggles() {
    let mut f = fx(Workspace::single(tid(1)));
    let effects = f.run(&bare_action("last-pane"));
    assert!(
        effects.bell && !effects.layout_mutated && effects.set_focus.is_none(),
        "no history"
    );

    f.workspace.add_window("2".to_owned(), tid(2));
    f.workspace.select(0);
    f.focus_history = FocusHistory::with_previous(tid(2));
    let effects = f.run(&bare_action("last-pane"));
    assert_eq!(f.workspace.active, 1);
    assert_eq!(
        f.workspace.active_window().and_then(|w| w.focus.clone()),
        Some(tid(2))
    );
    assert!(
        effects.clear_predict && !effects.set_metadata,
        "focus MRU is client-local"
    );
    let mut focused = Some(tid(1));
    let mut history = FocusHistory::with_previous(tid(2));
    history.transition(&mut focused, effects.set_focus);
    assert_eq!(history.previous(), Some(&tid(1)));

    // Feeding the recorded pane back toggles and repairs the MRU.
    f.focus_history = FocusHistory::with_previous(tid(1));
    let effects = f.run(&bare_action("last-pane"));
    assert_eq!(f.workspace.active, 0);
    history.transition(&mut focused, effects.set_focus);
    assert_eq!(focused, Some(tid(1)));
    assert_eq!(history.previous(), Some(&tid(2)));
}

/// `move-window` moves the active window (keeping it active) and broadcasts;
/// `delta` and `index` both clamp at the ends; no destination bells.
#[test]
fn move_window_moves_the_active_window_and_clamps() {
    let mut workspace = three_windows();
    let effects = run(
        &act("move-window", &[("delta", (-1).into())]),
        &mut workspace,
    );
    assert_eq!(names(&workspace), ["1", "3", "2"]);
    assert_eq!(workspace.active, 1);
    assert!(effects.layout_mutated && effects.set_metadata && !effects.bell);

    run(
        &act("move-window", &[("delta", (-9).into())]),
        &mut workspace,
    );
    assert_eq!(names(&workspace), ["3", "1", "2"]);
    let effects = run(
        &act("move-window", &[("delta", (-1).into())]),
        &mut workspace,
    );
    assert!(effects.bell && !effects.set_metadata, "already first");

    for index in [2, 99] {
        let mut workspace = three_windows();
        workspace.select(0);
        let effects = run(
            &act("move-window", &[("index", index.into())]),
            &mut workspace,
        );
        assert_eq!(names(&workspace), ["2", "3", "1"]);
        assert_eq!(workspace.active, 2);
        assert!(effects.set_metadata);
    }

    let mut workspace = three_windows();
    let effects = run(&bare_action("move-window"), &mut workspace);
    assert!(effects.bell && !effects.layout_mutated);
    assert_eq!(names(&workspace), ["1", "2", "3"]);
}

/// toggle-zoom requests the flip on a multi-pane window and bells on a
/// single pane; toggle-sidebar always requests its flip.
#[test]
fn toggle_zoom_and_sidebar_request_their_flips() {
    let effects = run(&bare_action("toggle-zoom"), &mut two_pane_workspace());
    assert!(effects.toggle_zoom && effects.layout_mutated && !effects.bell);
    let effects = run(&bare_action("toggle-zoom"), &mut Workspace::single(tid(1)));
    assert!(effects.bell && !effects.toggle_zoom && !effects.layout_mutated);
    let effects = run(
        &bare_action("toggle-sidebar"),
        &mut Workspace::single(tid(1)),
    );
    assert!(
        effects.toggle_sidebar && effects.layout_mutated && !effects.bell && !effects.toggle_zoom
    );
}

/// Applying `toggle_sidebar` flips the driver-owned flag, off to on and back.
#[tokio::test]
async fn apply_effects_flips_sidebar_enabled_state() {
    let mut f = CtxFixture::default();
    for expected in [true, false] {
        let effects = f.run(&bare_action("toggle-sidebar"));
        f.apply(effects).await;
        assert_eq!(f.sidebar_enabled, expected);
    }
}

fn root_ratio(workspace: &Workspace) -> f32 {
    match workspace.active_window().unwrap().tree.as_ref().unwrap() {
        LayoutNode::Split { ratio, .. } => *ratio,
        other @ LayoutNode::Leaf(_) => panic!("expected root Split, got {other:?}"),
    }
}

/// `resize-pane` moves the ratio by amount/axis-cells and broadcasts; missing
/// args or a squeeze below the 2-cell floor bell without mutating
/// (ADR-0019 decision 5).
#[test]
fn resize_pane_moves_ratio_or_bells() {
    let resize = |amount: i64| {
        act(
            "resize-pane",
            &[("direction", "right".into()), ("amount", amount.into())],
        )
    };
    let mut workspace = two_pane_workspace();
    let effects = run(&resize(8), &mut workspace);
    assert!(!effects.bell && effects.layout_mutated && effects.set_metadata);
    assert!(
        (root_ratio(&workspace) - 0.6).abs() < 1e-4,
        "{}",
        root_ratio(&workspace)
    );

    for action in [bare_action("resize-pane"), resize(80)] {
        let mut workspace = two_pane_workspace();
        let effects = run(&action, &mut workspace);
        assert!(effects.bell && !effects.layout_mutated && !effects.set_metadata);
        assert!((root_ratio(&workspace) - 0.5).abs() < f32::EPSILON);
    }
}

/// rename-window with a name renames and broadcasts; without one it opens a
/// prompt and broadcasts nothing until commit.
#[test]
fn rename_window_renames_or_prompts() {
    let mut workspace = Workspace::single(tid(1));
    let effects = run(
        &act("rename-window", &[("name", "build".into())]),
        &mut workspace,
    );
    assert_eq!(workspace.windows[0].name, "build");
    assert!(effects.layout_mutated && effects.set_metadata);

    let mut f = fx(Workspace::single(tid(1)));
    let effects = f.run(&bare_action("rename-window"));
    assert!(f.overlays.is_active() && effects.layout_mutated && !effects.set_metadata);
    assert_eq!(f.workspace.windows[0].name, "1");
}

// ---- directories and satellites ---------------------------------------------

/// `go-to-directory` lists the explicit `path`, else the home request, on
/// the attached server; the placeholder overlay needs a repaint.
#[test]
fn go_to_directory_requests_a_listing() {
    let effects = run(
        &bare_action("go-to-directory"),
        &mut Workspace::single(tid(1)),
    );
    let (pending, frame) = effects.list_directory.expect("LIST_DIRECTORY");
    assert_eq!(
        frame,
        FrameKind::ListDirectory {
            request_id: pending.request_id,
            path: String::new(),
            host: None
        }
    );
    assert_eq!(pending.host, ListingHost::Attached);

    let effects = run(
        &act("go-to-directory", &[("path", "/srv".into())]),
        &mut Workspace::single(tid(1)),
    );
    let (_, frame) = effects.list_directory.expect("LIST_DIRECTORY");
    assert!(matches!(&frame, FrameKind::ListDirectory { path, .. } if path == "/srv"));
    assert!(!effects.bell && effects.layout_mutated);

    // Without `LIST_DIRECTORY` the older server would drop the frame: bell.
    let mut f = fx(Workspace::single(tid(1)));
    f.directory_support =
        DirectorySupport::from_features(ServerFeatureSet::with(&[ServerFeature::SpawnInitialSize]));
    let effects = f.run(&bare_action("go-to-directory"));
    assert!(effects.list_directory.is_none() && effects.bell);
}

/// On a satellite pane the listing asks that satellite through the hub, at
/// the pane's directory (or its home); an older hub keeps it on itself.
#[test]
fn go_to_directory_on_a_satellite_pane_names_its_host() {
    for (cwd, path) in [(Some("/home/e/src"), "/home/e/src"), (None, "")] {
        let (mut f, panes) = satellite_pane(cwd);
        let (pending, frame) = f
            .run_in(&bare_action("go-to-directory"), &panes)
            .list_directory
            .expect("LIST");
        assert_eq!(
            frame,
            FrameKind::ListDirectory {
                request_id: pending.request_id,
                path: path.to_owned(),
                host: Some(edge())
            }
        );
        assert_eq!(pending.host, ListingHost::Satellite(edge()));
    }

    let (mut f, panes) = satellite_pane(Some("/home/e/src"));
    older_hub(&mut f);
    let (pending, frame) = f
        .run_in(&bare_action("go-to-directory"), &panes)
        .list_directory
        .expect("LIST");
    assert_eq!(
        frame,
        FrameKind::ListDirectory {
            request_id: pending.request_id,
            path: String::new(),
            host: None
        }
    );
    assert_eq!(pending.host, ListingHost::AttachedInsteadOf(edge()));
}

/// Splitting a satellite pane spawns on that satellite at its cwd (bound);
/// a local pane's split is unchanged; an older hub keeps it local and
/// remembers the satellite it stands in for.
#[test]
fn split_pane_follows_the_focused_panes_host() {
    let (mut f, panes) = satellite_pane(Some("/home/e/src"));
    let (_, pending, frame) = f
        .run_in(&split_action(), &panes)
        .spawn_terminal
        .expect("SPAWN");
    assert!(
        matches!(&frame, FrameKind::SpawnResource { cwd: Some(c), satellite: Some(h), .. } if c == "/home/e/src" && *h == edge()),
        "{frame:?}"
    );
    assert_eq!(
        (pending.host, pending.adopt),
        (SplitHost::Satellite(edge()), None)
    );
    assert!(asks_binding(&frame));

    let mut slot = PaneSlot::new().expect("slot");
    slot.cwd = Some("/srv/app".to_owned());
    let panes = HashMap::from([(tid(1), slot)]);
    let (_, pending, frame) = fx(Workspace::single(tid(1)))
        .run_in(&split_action(), &panes)
        .spawn_terminal
        .expect("SPAWN");
    assert!(
        matches!(
            &frame,
            FrameKind::SpawnResource {
                cwd: None,
                satellite: None,
                ..
            }
        ),
        "{frame:?}"
    );
    assert_eq!(pending.host, SplitHost::Attached);
    assert!(!asks_binding(&frame));

    let (mut f, panes) = satellite_pane(Some("/home/e/src"));
    older_hub(&mut f);
    let (_, pending, frame) = f
        .run_in(&split_action(), &panes)
        .spawn_terminal
        .expect("SPAWN");
    assert!(
        matches!(
            &frame,
            FrameKind::SpawnResource {
                cwd: None,
                satellite: None,
                ..
            }
        ),
        "{frame:?}"
    );
    assert_eq!(pending.host, SplitHost::AttachedInsteadOf(edge()));
    assert!(!asks_binding(&frame));
}

/// `split-pane { host }` spawns on that satellite with no owner; `split-pane
/// { resource = "host/@N" }` attaches that existing pane into the current
/// window instead of spawning (the leaf appears when the attach succeeds).
#[test]
fn split_onto_a_host_or_an_existing_satellite_pane() {
    let devbox = SatelliteHost::new("devbox");
    let mut action = split_action();
    action.args.insert("host".to_owned(), "devbox".into());
    let (_, pending, frame) = run(&action, &mut Workspace::single(tid(1)))
        .spawn_terminal
        .expect("SPAWN");
    assert!(
        matches!(&frame, FrameKind::SpawnResource { satellite: Some(h), owner_terminal: None, .. } if *h == devbox),
        "{frame:?}"
    );
    assert_eq!(pending.host, SplitHost::Satellite(devbox));
    assert!(pending.adopt.is_none() && pending.open_existing.is_none());
    assert!(asks_binding(&frame));

    let mut action = split_action();
    action
        .args
        .insert("resource".to_owned(), "devbox/@7".into());
    let mut workspace = Workspace::single(tid(1));
    let effects = run(&action, &mut workspace);
    let target = satellite_id("devbox", 7);
    let (_, pending, frame) = effects.spawn_terminal.expect("open parks an ATTACH");
    assert_eq!(pending.open_existing.as_ref(), Some(&target));
    assert!(pending.adopt.is_none() && effects.spawn_window.is_none());
    assert!(
        matches!(&frame, FrameKind::Command { command: Command::AttachResource { terminal_id, .. }, .. } if *terminal_id == target),
        "{frame:?}"
    );
    assert_eq!(
        crate::layout::leaves(workspace.active_window().unwrap().tree.as_ref().unwrap()),
        vec![tid(1)]
    );
}

// ---- plugins and overlays ---------------------------------------------------

#[test]
fn plugin_action_records_run_intent_or_bells() {
    let action = act(
        "plugin-action",
        &[
            ("plugin", "com.example.tools".into()),
            ("action", "summarize".into()),
        ],
    );
    let effects = run(&action, &mut Workspace::single(tid(1)));
    assert_eq!(
        effects.run_plugin,
        Some(("com.example.tools".to_owned(), "summarize".to_owned()))
    );
    assert!(
        !effects.bell && !effects.layout_mutated,
        "the async caller spawns the run"
    );

    let effects = run(
        &bare_action("plugin-action"),
        &mut Workspace::single(tid(1)),
    );
    assert!(effects.bell && effects.run_plugin.is_none());
}

fn board(placement: HostedPlacement) -> PluginPaneEntry {
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

fn run_plugin_pane(
    plugin: &str,
    placement: HostedPlacement,
    workspace: Workspace,
) -> ActionEffects {
    let mut f = fx(workspace);
    f.plugin_panes = vec![board(placement)];
    f.run(&act(
        "plugin-pane",
        &[("plugin", plugin.into()), ("pane", "board".into())],
    ))
}

/// Split/zoomed placements park a split running the manifest argv with the
/// identity env; a tab placement parks a window named after the title.
#[test]
fn plugin_pane_placement_routes_the_spawn() {
    let argv = Some(vec!["agent-board".to_owned(), "--watch".to_owned()]);
    for (placement, zoom) in [
        (HostedPlacement::Split, false),
        (HostedPlacement::Zoomed, true),
    ] {
        let effects = run_plugin_pane("com.example.board", placement, Workspace::single(tid(1)));
        let (_, pending, frame) = effects.spawn_terminal.expect("parks a split");
        assert_eq!(pending.focused_at_request, tid(1));
        assert_eq!(pending.zoom_on_spawn, zoom);
        assert!(effects.spawn_window.is_none());
        let FrameKind::SpawnResource {
            command, cwd, env, ..
        } = frame
        else {
            panic!("spawn")
        };
        assert_eq!(command, argv);
        assert_eq!(cwd.as_deref(), Some("/plugins/board"));
        let env = env.expect("identity env injected");
        for pair in [
            ("PHUX_PLUGIN_ID", "com.example.board"),
            ("PHUX_PLUGIN_PANE_ID", "board"),
            ("PHUX_PLUGIN_ROOT", "/plugins/board"),
        ] {
            assert!(
                env.contains(&(pair.0.to_owned(), pair.1.to_owned())),
                "{pair:?}"
            );
        }
    }
    let effects = run_plugin_pane(
        "com.example.board",
        HostedPlacement::Tab,
        Workspace::single(tid(1)),
    );
    let (_, pending, frame) = effects.spawn_window.expect("tab parks a window");
    assert_eq!(pending.name, "Agent Board");
    assert!(effects.spawn_terminal.is_none());
    assert!(matches!(frame, FrameKind::SpawnResource { command, .. } if command == argv));
}

/// An unknown entry (disabled plugin, typo, overlay declaration) or a split
/// with no focused pane bells.
#[test]
fn plugin_pane_unknown_or_unfocused_bells() {
    for (plugin, workspace) in [
        ("com.example.absent", Workspace::single(tid(1))),
        ("com.example.board", Workspace::default()),
    ] {
        let effects = run_plugin_pane(plugin, HostedPlacement::Split, workspace);
        assert!(
            effects.bell && effects.spawn_terminal.is_none() && effects.spawn_window.is_none(),
            "{plugin}"
        );
    }
}

/// A palette row's action, fed back through `run_action`, has the same
/// effect a keybinding does.
#[test]
fn palette_committed_action_routes_through_run_action() {
    let cfg = default_cfg();
    let items = crate::attach::action_registry::palette_items(Some(&cfg.keybindings), &[], &[]);
    let detach = items
        .iter()
        .find(|i| i.action.action == "detach")
        .expect("detach in palette");
    assert!(run(&detach.action, &mut Workspace::default()).detach);
}

/// Each overlay-opening action pushes one overlay without a repaint or bell;
/// guidance is passthrough, the settings page captures input.
#[test]
fn overlay_actions_push_their_overlay() {
    for (action, passthrough) in [
        ("show-help", false),
        ("command-palette", false),
        ("settings", false),
        ("getting-started", true),
        ("agent-fleet", false),
    ] {
        let mut f = fx(Workspace::single(tid(1)));
        let effects = f.run(&bare_action(action));
        assert_eq!(f.overlays.depth(), 1, "{action}");
        assert_eq!(f.overlays.top_is_passthrough(), passthrough, "{action}");
        assert!(
            !effects.layout_mutated && !effects.bell && !effects.reload_config,
            "{action}"
        );
    }
    let mut f = fx(Workspace::single(tid(1)));
    f.workspace.add_window("2".to_owned(), tid(2));
    assert!(!f.run(&bare_action("window-picker")).bell);
    assert!(f.overlays.is_active());
    for action in ["window-picker", "agent-fleet"] {
        let mut f = fx(Workspace::default());
        assert!(
            f.run(&bare_action(action)).bell,
            "{action}: nothing to list"
        );
        assert!(!f.overlays.is_active());
    }
}

/// The fleet overlay is keyed for the driver's live refresh; a static overlay
/// (the palette) ignores it.
#[test]
fn only_the_fleet_overlay_accepts_a_live_fleet_refresh() {
    use crate::attach::fleet::{FLEET_LIVE_KEY, fleet_items};

    let workspace = Workspace::single(tid(1));
    let fresh = fleet_items(
        &workspace,
        &[],
        None,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    let mut f = fx(workspace.clone());
    f.run(&bare_action("agent-fleet"));
    assert!(f.overlays.refresh_items(FLEET_LIVE_KEY, &fresh));
    let mut f = fx(workspace);
    f.run(&bare_action("command-palette"));
    assert!(!f.overlays.refresh_items(FLEET_LIVE_KEY, &[]));
}

// ---- window picker and focus-pane ---------------------------------------------

#[test]
fn current_session_window_rows_commit_select_window() {
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("editor".to_owned(), tid(2));
    workspace.select(0);
    let items = current_session_window_rows(&workspace);
    assert_eq!(items.len(), 2);
    assert_eq!(
        (items[0].label.as_str(), items[0].secondary.as_deref()),
        ("0:1", Some("1 pane"))
    );
    assert!(items[0].indented, "window rows nest under their session");
    assert_eq!(items[1].label, "1:editor");
    assert_eq!(items[1].action.args.get("index"), Some(&Value::Integer(1)));
    // Committing a row performs the per-client switch a binding does.
    let effects = run(&items[1].action, &mut workspace);
    assert_eq!(workspace.active, 1);
    assert!(effects.layout_mutated && !effects.set_metadata);
    assert_eq!(effects.set_focus, Some(tid(2)));
}

/// The picker leads with the current session's windows; a peer with a
/// cached layout lists its windows as one-step `switch-session { name,
/// window }` rows, else (no or an empty layout) one plain switch row.
#[test]
fn window_picker_groups_windows_under_their_session() {
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("editor".to_owned(), tid(2));
    let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
    let mut scratch = Workspace::single(tid(10));
    scratch.rename_active("build".to_owned());
    scratch.add_window("logs".to_owned(), tid(11));

    for (cached, rows) in [
        (None, 0),
        (Some(Workspace::default()), 0),
        (Some(scratch), 2),
    ] {
        let foreign: HashMap<_, _> = cached
            .into_iter()
            .map(|ws| (SessionId::new(2), ws))
            .collect();
        let items = window_picker_items(&workspace, &sessions, &foreign, Some(SessionId::new(1)));
        assert!(items[0].is_header());
        assert_eq!(items[0].label, "work (current)");
        assert!(!items[1].is_header() && items[1].indented);
        assert_eq!(items[1].action.action, "select-window");
        assert_eq!(items[2].action.action, "select-window");
        let at = items
            .iter()
            .position(|i| i.is_header() && i.label == "scratch")
            .expect("header");
        if rows == 0 {
            assert_eq!(items[at + 1].label, "switch to this session");
            assert_eq!(
                items[at + 1].action.args.get("name"),
                Some(&Value::String("scratch".to_owned()))
            );
            assert!(!items[at + 1].action.args.contains_key("window"));
            continue;
        }
        for (i, label) in ["0:build", "1:logs"].into_iter().enumerate() {
            let row = &items[at + 1 + i];
            assert_eq!(row.label, label);
            assert!(row.indented);
            assert_eq!(row.action.action, "switch-session");
            assert_eq!(
                row.action.args.get("window"),
                Some(&Value::Integer(i64::try_from(i).unwrap()))
            );
        }
        assert_eq!(items[at + 1].secondary.as_deref(), Some("1 pane"));
        assert!(items.iter().all(|i| i.label != "switch to this session"));
    }
}

#[test]
fn focus_pane_switches_window_and_focuses_leaf() {
    let focus_pane = |pane: i64| act("focus-pane", &[("window", 0.into()), ("pane", pane.into())]);
    for select_first in [false, true] {
        let mut workspace = fleet_workspace();
        if select_first {
            workspace.select(0);
        }
        let effects = run(&focus_pane(1), &mut workspace);
        assert_eq!(workspace.active, 0);
        assert_eq!(workspace.windows[0].state.focus, Some(tid(2)));
        assert_eq!(effects.set_focus, Some(tid(2)));
        assert!(effects.layout_mutated && !effects.set_metadata && !effects.bell);
    }
    // Missing args or a stale address bell without switching windows.
    for action in [bare_action("focus-pane"), focus_pane(9)] {
        let mut workspace = fleet_workspace();
        let effects = run(&action, &mut workspace);
        assert!(effects.bell && effects.set_focus.is_none());
        assert_eq!(workspace.active, 1);
    }
    // A fleet row commits the same focus-pane.

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
    let effects = run(&items[1].action.clone(), &mut workspace);
    assert_eq!((workspace.active, effects.set_focus), (0, Some(tid(2))));
}

// ---- attention navigation ----------------------------------------------------

fn asking_panes(ids: &[u32]) -> HashMap<ResourceId, PaneSlot> {
    ids.iter()
        .map(|id| {
            let mut slot = PaneSlot::new_with_size(20, 4).expect("pane slot");
            slot.attention = true;
            (tid(*id), slot)
        })
        .collect()
}

fn attention_fixture() -> CtxFixture {
    let mut workspace = fleet_workspace();
    workspace.select(0);
    fx(workspace)
}

/// Client-local attention navigation: with nothing asking it bells and saves
/// no origin; otherwise it cycles DFS across windows with wrap, keeps the
/// first origin, and one return consumes it. Nothing is ever broadcast.
#[test]
fn next_attention_cycles_and_return_consumes_one_origin() {
    let mut f = attention_fixture();
    let before = f.workspace.clone();
    let effects = f.run_in(&bare_action("next-attention"), &HashMap::new());
    assert!(effects.bell && !effects.layout_mutated && !effects.set_metadata);
    assert_eq!(f.workspace, before);
    assert!(f.attention_navigation.take_origin().is_none());

    let panes = asking_panes(&[2, 3]);
    for (active, focus) in [(0, 2), (1, 3), (0, 2)] {
        let effects = f.run_in(&bare_action("next-attention"), &panes);
        assert_eq!(
            (f.workspace.active, effects.set_focus),
            (active, Some(tid(focus)))
        );
        assert!(!effects.set_metadata);
    }
    let returned = f.run_in(&bare_action("return-from-attention"), &panes);
    assert_eq!(
        returned.set_focus,
        Some(tid(1)),
        "cycling kept the first origin"
    );
    assert_eq!(f.workspace.windows[0].state.focus, Some(tid(1)));
    assert!(!returned.set_metadata);
    let consumed = f.run_in(&bare_action("return-from-attention"), &panes);
    assert!(consumed.bell && !consumed.layout_mutated && !consumed.set_metadata);
}

/// The origin pane closed while the user looked at the question: return
/// bells, focuses nothing else, and still consumes the stale origin.
#[test]
fn return_from_attention_consumes_a_stale_origin_safely() {
    let mut f = attention_fixture();
    let panes = asking_panes(&[2]);
    assert_eq!(
        f.run_in(&bare_action("next-attention"), &panes).set_focus,
        Some(tid(2))
    );
    f.workspace.windows[0].state = LayoutState::single(tid(2));
    let before = f.workspace.clone();
    for _ in 0..2 {
        let effects = f.run_in(&bare_action("return-from-attention"), &panes);
        assert!(effects.bell && !effects.layout_mutated && effects.set_focus.is_none());
        assert_eq!(f.workspace, before);
    }
}

// ---- sessions ----------------------------------------------------------------

#[allow(
    clippy::unnecessary_wraps,
    reason = "compared against `effects.reattach`"
)]
fn existing(
    name: &str,
    id: Option<u32>,
    window: Option<usize>,
    pane: Option<usize>,
    resource: Option<ResourceId>,
) -> Option<ReattachTarget> {
    Some(ReattachTarget::Existing {
        name: name.to_owned(),
        id: id.map(SessionId::new),
        window,
        pane,
        resource,
    })
}

#[test]
fn session_picker_items_include_focused_first_and_commit_switch_session() {
    let sessions = [sinfo(1, "work"), sinfo(2, "scratch"), sinfo(3, "logs")];
    let items = session_picker_items(&sessions, Some(SessionId::new(1)));
    let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["work", "logs", "scratch"]);
    assert_eq!(items[0].secondary.as_deref(), Some("1 window, current"));
    assert_eq!(items[1].secondary.as_deref(), Some("1 window"));
    assert_eq!(items[0].action.action, "switch-session");
    assert_eq!(
        items[0].action.args.get("name"),
        Some(&Value::String("work".to_owned()))
    );
    assert_eq!(items[0].action.args.get("id"), Some(&Value::Integer(1)));
    // Committing a peer row requests the switch by name and stable id.
    let effects = run(&items[2].action, &mut Workspace::single(tid(1)));
    assert_eq!(
        effects.reattach,
        existing("scratch", Some(2), None, None, None)
    );
}

/// The session picker always opens (its "+ New session" row is never a dead
/// end) and asks the driver for a fresh host inventory.
#[test]
fn session_picker_always_opens_and_requests_a_host_inventory() {
    for sessions in [
        vec![],
        vec![sinfo(1, "work")],
        vec![sinfo(1, "work"), sinfo(2, "scratch")],
    ] {
        let mut f = fx(Workspace::single(tid(1)));
        f.focused_session = (!sessions.is_empty()).then(|| SessionId::new(1));
        f.sessions = sessions;
        assert!(!f.run(&bare_action("session-picker")).bell);
        assert!(f.overlays.is_active() && f.host_refresh_request);
    }
}

/// `switch-session` args: `window`/`pane` carry the one-step target (a bad
/// `window` degrades to a plain switch), `id` outranks a stale name, and
/// `resource` names local or satellite identity. No name bells.
#[test]
fn switch_session_args_build_the_reattach_target() {
    let mut scratch = Workspace::single(tid(10));
    scratch.add_window("logs".to_owned(), tid(11));
    let rows = foreign_session_window_rows(&sinfo(2, "scratch"), &scratch);
    assert_eq!(rows.len(), 2);
    let cases = [
        (
            rows[1].action.clone(),
            existing("scratch", Some(2), Some(1), None, None),
        ),
        (
            act(
                "switch-session",
                &[("name", "scratch".into()), ("window", (-3).into())],
            ),
            existing("scratch", None, None, None, None),
        ),
        (
            act(
                "switch-session",
                &[
                    ("name", "scratch".into()),
                    ("window", 1.into()),
                    ("pane", 2.into()),
                ],
            ),
            existing("scratch", None, Some(1), Some(2), None),
        ),
        (
            act(
                "switch-session",
                &[("name", "stale".into()), ("id", 7.into())],
            ),
            existing("stale", Some(7), None, None, None),
        ),
        (
            act(
                "switch-session",
                &[
                    ("name", "peer".into()),
                    ("id", 2.into()),
                    ("resource", "@10".into()),
                ],
            ),
            existing("peer", Some(2), None, None, Some(tid(10))),
        ),
        (
            act(
                "switch-session",
                &[("name", "peer".into()), ("resource", "prod-3/@10".into())],
            ),
            existing("peer", None, None, None, Some(satellite_id("prod-3", 10))),
        ),
    ];
    for (action, target) in cases {
        let mut workspace = Workspace::single(tid(1));
        let effects = run(&action, &mut workspace);
        assert_eq!(effects.reattach, target, "{:?}", action.args);
        assert!(!effects.bell);
        assert_eq!(
            workspace.active, 0,
            "a switch is a re-attach, not a local change"
        );
    }
    let effects = run(
        &bare_action("switch-session"),
        &mut Workspace::single(tid(1)),
    );
    assert!(effects.reattach.is_none() && effects.bell);
}

/// new-session with a name creates and switches; without one it prompts.
#[test]
fn new_session_creates_or_prompts() {
    let effects = run(
        &act("new-session", &[("name", "scratch".into())]),
        &mut Workspace::single(tid(1)),
    );
    assert_eq!(
        effects.reattach,
        Some(ReattachTarget::Create("scratch".to_owned()))
    );
    let mut f = fx(Workspace::single(tid(1)));
    assert!(f.run(&bare_action("new-session")).reattach.is_none());
    assert!(f.overlays.is_active());
}

#[test]
fn detach_action_requests_detach_effect() {
    let effects = run(&bare_action("detach"), &mut Workspace::default());
    assert!(effects.detach && !effects.layout_mutated);
}

/// The move-pane picker offers only exact cached local destinations (not the
/// source, satellites, or uncached sessions), and a row commits the move.
#[test]
fn move_pane_picker_offers_only_exact_cached_local_destinations() {
    let source = tid(1);
    let mut current = Workspace {
        windows: vec![WindowState::new(
            "editor".to_owned(),
            LayoutState {
                tree: Some(LayoutNode::Split {
                    dir: SplitDir::Horizontal,
                    ratio: 0.5,
                    left: Box::new(LayoutNode::Leaf(source.clone())),
                    right: Box::new(LayoutNode::Split {
                        dir: SplitDir::Vertical,
                        ratio: 0.5,
                        left: Box::new(LayoutNode::Leaf(tid(2))),
                        right: Box::new(LayoutNode::Leaf(satellite_id("edge", 9))),
                    }),
                }),
                focus: Some(source.clone()),
            },
        )],
        active: 0,
    };
    let mut cached = Workspace::single(tid(3));
    cached.windows[0].name = "tests".to_owned();
    let sessions = [
        SessionInfo::new(SessionId::new(1), "work"),
        SessionInfo::new(SessionId::new(2), "build"),
        SessionInfo::new(SessionId::new(3), "uncached"),
    ];
    let foreign = HashMap::from([(SessionId::new(2), cached)]);
    let rows = move_pane_picker_items(
        &source,
        &current,
        "work",
        Some(SessionId::new(1)),
        &sessions,
        &foreign,
    );
    assert_eq!(
        rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
        ["@2", "@3"]
    );
    assert_eq!(
        rows.iter()
            .map(|r| r.secondary.as_deref().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["work · 0:editor · pane 2", "build · 0:tests · pane 1"]
    );
    assert_eq!(rows[1].action.action, "move-pane");
    assert_eq!(rows[1].action.args["target"].as_integer(), Some(3));
    assert_eq!(rows[1].action.args.len(), 1);

    let intent = run(&rows[1].action, &mut current)
        .move_pane
        .expect("commits a move");
    assert_eq!(
        (intent.source, intent.target, intent.dir),
        (tid(1), tid(3), SplitDir::Horizontal)
    );
    assert_eq!(intent.ratio.to_bits(), 0.5_f32.to_bits());

    let mut workspace = Workspace::single(tid(1));
    let effects = run(&bare_action("move-pane"), &mut workspace);
    assert!(effects.bell && effects.move_pane.is_none());
    assert_eq!(workspace, Workspace::single(tid(1)));
}

// ---- host-grouped session picker ----------------------------------------------

/// One reachable satellite with two sessions, one idle, and one unreachable.
fn host_fixture() -> Vec<HostInventory> {
    let session = |id, name, windows, panes, active| {
        HostSessionInfo::new(phux_protocol::SessionId::new(id), name)
            .with_window_count(windows)
            .with_pane_count(panes)
            .with_active_resource(Some(satellite_id("edge", active)))
    };
    vec![
        HostInventory::reachable(
            edge(),
            vec![session(1, "build", 2, 3, 9), session(2, "logs", 1, 1, 11)],
        ),
        HostInventory::reachable(SatelliteHost::new("idle"), Vec::new()),
        HostInventory::unreachable(SatelliteHost::new("down"), "link is down"),
    ]
}

/// The picker groups under a header per host, keeps an unreachable host
/// visible, commits `switch-session { name, host }` for a satellite, and
/// marks a satellite session already open here. No inventory: ungrouped.
#[test]
fn session_picker_groups_rows_by_host() {
    let sessions = [sinfo(1, "work")];
    let items = host_grouped_session_items(
        &sessions,
        Some(SessionId::new(1)),
        &host_fixture(),
        &Workspace::single(tid(1)),
    );
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Local",
            "work",
            "edge - 2 sessions",
            "build",
            "logs",
            "idle - connected, no sessions",
            "down - unreachable: link is down"
        ]
    );
    for i in [0, 2, 5, 6] {
        assert!(items[i].is_header(), "{i}");
    }
    assert!(items[1].indented && items[3].indented);
    assert!(!items[1].action.args.contains_key("host"));
    assert_eq!(items[3].action.action, "switch-session");
    assert_eq!(
        items[3].action.args.get("host"),
        Some(&Value::String("edge".to_owned()))
    );
    assert_eq!(
        items[3].action.args.get("name"),
        Some(&Value::String("build".to_owned()))
    );
    assert_eq!(
        items[3].secondary.as_deref(),
        Some("on edge, 2 windows, 3 panes")
    );

    let mut open = Workspace::single(tid(1));
    open.add_window("edge/build".to_owned(), satellite_id("edge", 9));
    let items = host_grouped_session_items(&[], None, &host_fixture(), &open);
    let build = items.iter().find(|i| i.label == "build").expect("row");
    assert!(
        build
            .secondary
            .as_deref()
            .is_some_and(|s| s.contains("open here")),
        "{:?}",
        build.secondary
    );

    let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
    let grouped = host_grouped_session_items(
        &sessions,
        Some(SessionId::new(1)),
        &[],
        &Workspace::single(tid(1)),
    );
    assert_eq!(
        grouped.len(),
        session_picker_items(&sessions, Some(SessionId::new(1))).len()
    );
    assert!(grouped.iter().all(|item| !item.is_header()));
}

fn switch_to_satellite(
    host: &str,
    name: &str,
    workspace: Workspace,
) -> (ActionEffects, CtxFixture) {
    let mut f = fx(workspace);
    f.hosts = host_fixture();
    let effects = f.run(&act(
        "switch-session",
        &[("name", name.into()), ("host", host.into())],
    ));
    (effects, f)
}

/// A satellite session opens through the hub: one `ATTACH_RESOURCE` for its
/// active pane and a parked window, with no re-attach and nothing opened
/// until the attach succeeds. Already open here, it focuses that window.
#[test]
fn switching_to_a_satellite_session_opens_or_focuses_its_pane() {
    let (effects, f) = switch_to_satellite("edge", "build", Workspace::single(tid(1)));
    assert!(effects.reattach.is_none());
    assert_eq!(f.workspace.windows.len(), 1);
    assert!(effects.set_focus.is_none() && !effects.layout_mutated && !effects.set_metadata);
    let [FrameKind::Command { command, .. }] = effects.command_frames.as_slice() else {
        panic!("expected one command frame: {:?}", effects.command_frames);
    };
    assert_eq!(
        command,
        &Command::AttachResource {
            terminal_id: satellite_id("edge", 9),
            role_policy: None
        }
    );

    let mut open = Workspace::single(tid(1));
    open.add_window("edge/build".to_owned(), satellite_id("edge", 9));
    open.select(0);
    let (effects, f) = switch_to_satellite("edge", "build", open);
    assert_eq!((f.workspace.windows.len(), f.workspace.active), (2, 1));
    assert_eq!(effects.set_focus, Some(satellite_id("edge", 9)));
    assert!(effects.command_frames.is_empty());
}

/// An unreachable host, an unknown host, and an unknown session all bell.
#[test]
fn switching_to_an_unreachable_or_unknown_satellite_session_bells() {
    for (host, name) in [
        ("down", "anything"),
        ("nosuch", "build"),
        ("edge", "nosuch"),
    ] {
        let (effects, f) = switch_to_satellite(host, name, Workspace::single(tid(1)));
        assert!(
            effects.bell && effects.reattach.is_none() && effects.command_frames.is_empty(),
            "{host}/{name}"
        );
        assert_eq!(f.workspace.windows.len(), 1);
    }
}

// ---- rename-session --------------------------------------------------------------

/// With a name, rename-session requests the effect; without one it opens a
/// prompt prefilled with the current name, whose commit yields the effect.
#[test]
fn rename_session_requests_the_effect_or_prompts() {
    use crate::render::overlay::{OverlayCommand, RenderOverlay};
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

    let effects = run(
        &act("rename-session", &[("name", "notes".into())]),
        &mut Workspace::single(tid(1)),
    );
    assert_eq!(effects.rename_session.as_deref(), Some("notes"));

    let mut f = fx(Workspace::single(tid(1)));
    f.session_name = "work".to_owned();
    assert!(
        f.run(&bare_action("rename-session"))
            .rename_session
            .is_none()
    );
    assert!(f.overlays.is_active());

    let mut prompt = PromptOverlay::rename_session("work", &Theme::default());
    let key = |key, text: Option<&str>| KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(ToOwned::to_owned),
        unshifted_codepoint: None,
    };
    for _ in 0..4 {
        let _ = prompt.handle_key(&key(PhysicalKey::Backspace, None));
    }
    for ch in ["n", "o", "t", "e", "s"] {
        let _ = prompt.handle_key(&key(PhysicalKey::A, Some(ch)));
    }
    let OverlayCommand::Commit(resolved) = prompt.handle_key(&key(PhysicalKey::Enter, None)) else {
        panic!("Enter on a non-empty prompt should commit");
    };
    let effects = run(&resolved, &mut Workspace::single(tid(1)));
    assert_eq!(effects.rename_session.as_deref(), Some("notes"));
}

fn rename_fixture(sessions: &[(u32, &str)]) -> CtxFixture {
    let mut f = fx(Workspace::single(tid(1)));
    f.session_name = "work".to_owned();
    f.focused_session = Some(SessionId::new(1));
    f.sessions = sessions
        .iter()
        .map(|(id, name)| SessionInfo::new(SessionId::new(*id), *name))
        .collect();
    f
}

/// A rename writes `SESSION_NAME_KEY` behind a correlated `GET_STATE` barrier and
/// does not touch the status name until the server confirms.
#[tokio::test(flavor = "current_thread")]
async fn rename_session_does_not_apply_locally_until_confirmed() {
    let mut f = rename_fixture(&[(1, "work")]);
    let effects = f.run(&act("rename-session", &[("name", "notes".into())]));
    let frames = f.apply(effects).await;
    assert_eq!(f.session_name, "work");
    let pending = f.rename_pending.expect("GET_STATE barrier parked");
    assert_eq!(
        (pending.current.as_str(), pending.new_name.as_str()),
        ("work", "notes")
    );
    assert_eq!(pending.session_id, Some(SessionId::new(1)));
    assert!(
        frames.iter().any(|f| matches!(f, FrameKind::SetMetadata { key, .. } if key == phux_protocol::wire::frame::SESSION_NAME_KEY)),
        "{frames:?}"
    );
    assert!(
        frames.iter().any(|f| matches!(f, FrameKind::Command { request_id, command: Command::GetState { .. } } if *request_id == pending.barrier)),
        "{frames:?}"
    );
}

/// A taken name is refused before anything is written.
#[tokio::test(flavor = "current_thread")]
async fn rename_session_refuses_a_taken_name_before_sending() {
    let mut f = rename_fixture(&[(1, "work"), (2, "notes")]);
    let effects = f.run(&act("rename-session", &[("name", "notes".into())]));
    let frames = f.apply(effects).await;
    assert!(f.rename_pending.is_none());
    assert!(
        f.rename_notice
            .expect("refusal notice")
            .contains("already exists")
    );
    assert!(frames.is_empty(), "{frames:?}");
}
