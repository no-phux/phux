fn view_rect(x: u16, y: u16, w: u16, h: u16) -> crate::layout::Rect {
    crate::layout::Rect { x, y, w, h }
}

/// A same-shaped tiling with a different holder is the window-switch / pane-
/// swap case: the rectangles match however the panes are ordered.
#[test]
fn same_rects_ignores_which_pane_holds_each_rectangle() {
    let (a, b) = (ResourceId::local(1), ResourceId::local(2));
    let two = |left: &ResourceId, right: &ResourceId| {
        HashMap::from([
            (left.clone(), view_rect(0, 1, 40, 20)),
            (right.clone(), view_rect(40, 1, 40, 20)),
        ])
    };
    let before = two(&a, &b);
    assert!(
        super::input::same_rects(&before, &two(&b, &a)),
        "holders do not matter"
    );
    // A different split is a different shape.
    let reshaped = |left: &ResourceId, right: &ResourceId| {
        HashMap::from([
            (left.clone(), view_rect(0, 1, 60, 20)),
            (right.clone(), view_rect(60, 1, 20, 20)),
        ])
    };
    assert!(!super::input::same_rects(&before, &reshaped(&a, &b)));
    // A different count never matches.
    assert!(!super::input::same_rects(
        &before,
        &HashMap::from([(a, view_rect(0, 1, 40, 20))])
    ));
}

/// The classification chooses the cheapest level that is still correct: a
/// focus move is chrome, a same-shaped window switch redraws panes in place
/// without a clear, and a moved layout (or a pane with no grid to draw) clears.
#[test]
fn classify_repaint_picks_the_minimal_level() {
    use super::input::classify_repaint;
    let (a, b, c) = (
        ResourceId::local(1),
        ResourceId::local(2),
        ResourceId::local(3),
    );
    let split = |left: &ResourceId, right: &ResourceId| {
        HashMap::from([
            (left.clone(), view_rect(0, 1, 40, 20)),
            (right.clone(), view_rect(40, 1, 40, 20)),
        ])
    };
    let before = split(&a, &b);
    // Identical view: only the focus (heavy borders) can have moved.
    assert_eq!(
        classify_repaint(&before, &before.clone(), |_| true),
        RepaintLevel::Chrome,
    );
    // Same rectangles, different panes: redraw in place, no clear.
    let swapped = split(&b, &c);
    assert_eq!(
        classify_repaint(&before, &swapped, |_| true),
        RepaintLevel::Panes,
    );
    // A pane the view cannot draw (no slot / no published replica) must clear.
    assert_eq!(
        classify_repaint(&before, &swapped, |id| *id != c),
        RepaintLevel::Full,
    );
    // A moved rectangle clears: the old cells would otherwise survive.
    let reshaped = HashMap::from([(a, view_rect(0, 1, 60, 20)), (b, view_rect(60, 1, 20, 20))]);
    assert_eq!(
        classify_repaint(&before, &reshaped, |_| true),
        RepaintLevel::Full,
    );
    // Nothing laid either side: nothing visible moved.
    let empty = HashMap::new();
    assert_eq!(
        classify_repaint(&empty, &empty, |_| true),
        RepaintLevel::Chrome,
    );
}

/// The headline flicker fix: selecting another window of the same shape
/// repaints its pane in place and never clears the screen, so a pane on the
/// alternate screen does not flash on a terminal that ignores DEC 2026.
#[tokio::test(flavor = "current_thread")]
async fn selecting_a_same_shaped_window_never_clears_the_screen() {
    use phux_config::keybind::parse_chord;

    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let config = phux_config::parse_with_defaults("", std::path::Path::new("t.toml"))
        .expect("shipped defaults parse");
    state.settings = crate::settings::TuiSettings::tolerant_from(&config);
    state.viewport_dims = (80, 24);

    let (a, b) = (ResourceId::local(1), ResourceId::local(2));
    let (kernel, _effects, panes) = crate::attach::pane_state::published_test_state(&[
        (&a, 60, 20, b"alpha screen\r\n"),
        (&b, 60, 20, b"beta screen\r\n"),
    ]);
    state.mirror.engine_kernel = kernel;
    state.mirror.panes = panes;
    let mut workspace = ArrangementDoc::single(a.clone());
    workspace.add_group("1".to_owned(), b.clone());
    workspace.select(0);
    state.mirror.workspace = workspace;
    state.mirror.focused_resource = Some(a.clone());
    state.layout_read_complete = true;
    drop(frames_sent(&mut client, &mut server).await);

    let press = |spec: &str| {
        let chord = parse_chord(spec).expect("chord");
        InputEvent::Key(KeyEvent {
            action: KeyAction::Press,
            key: chord.key,
            mods: chord.modifiers,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        })
    };
    let sidebar = state.sidebar();
    out.clear();
    state
        .dispatch_batch(
            &mut client,
            &mut out,
            sidebar,
            vec![press("C-a"), press("1")],
        )
        .await
        .expect("dispatch");

    assert_eq!(state.mirror.workspace.active, 1, "window 1 is now active");
    assert_eq!(state.mirror.focused_resource, Some(b.clone()));
    assert!(
        !out.windows(4).any(|s| s == b"\x1b[2J"),
        "a same-shaped window switch must not clear the screen: {:?}",
        String::from_utf8_lossy(&out)
    );
    assert!(
        !out.is_empty(),
        "the newly shown pane must still be repainted"
    );
    drop(client);
    drop(server);
}

/// Focusing another pane of the same split is chrome only: the heavy borders
/// move, but the panes are not re-rendered and the screen is never cleared.
#[tokio::test(flavor = "current_thread")]
async fn focusing_another_pane_never_clears_the_screen() {
    use crate::layout::{LayoutNode, LayoutState, SplitDir, split_at};
    use phux_config::keybind::parse_chord;

    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let config = phux_config::parse_with_defaults("", std::path::Path::new("t.toml"))
        .expect("shipped defaults parse");
    state.settings = crate::settings::TuiSettings::tolerant_from(&config);
    state.viewport_dims = (80, 24);

    let (a, b) = (ResourceId::local(1), ResourceId::local(2));
    let (kernel, _effects, panes) = crate::attach::pane_state::published_test_state(&[
        (&a, 30, 20, b"left pane\r\n"),
        (&b, 30, 20, b"right pane\r\n"),
    ]);
    state.mirror.engine_kernel = kernel;
    state.mirror.panes = panes;
    let tree = split_at(
        &LayoutNode::Leaf(a.clone()),
        &a,
        &b,
        SplitDir::Horizontal,
        0.5,
    )
    .expect("split");
    let mut workspace = ArrangementDoc::single(a.clone());
    workspace.groups[0].state = LayoutState {
        tree: Some(tree),
        focus: Some(a.clone()),
    };
    state.mirror.workspace = workspace;
    state.mirror.focused_resource = Some(a.clone());
    state.layout_read_complete = true;
    drop(frames_sent(&mut client, &mut server).await);

    let press = |spec: &str| {
        let chord = parse_chord(spec).expect("chord");
        InputEvent::Key(KeyEvent {
            action: KeyAction::Press,
            key: chord.key,
            mods: chord.modifiers,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        })
    };
    let sidebar = state.sidebar();
    out.clear();
    state
        .dispatch_batch(
            &mut client,
            &mut out,
            sidebar,
            vec![press("C-a"), press("o")],
        )
        .await
        .expect("dispatch");

    assert_eq!(
        state.mirror.focused_resource,
        Some(b.clone()),
        "next-pane moved the focus"
    );
    assert!(
        !out.windows(4).any(|s| s == b"\x1b[2J"),
        "a focus move must not clear the screen: {:?}",
        String::from_utf8_lossy(&out)
    );
    assert!(!out.is_empty(), "the heavy borders must repaint");
    drop(client);
    drop(server);
}

/// A wheel notch over a pane with history scrolls its viewport locally; no
/// tile moved, so the classifier's chrome-only level would leave the pane
/// showing the live tail. The batch must paint the history rows, without
/// clearing the screen and without a forced whole-pane repaint.
#[tokio::test(flavor = "current_thread")]
async fn a_wheel_scroll_paints_the_history_rows_without_clearing() {
    use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};

    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let config = phux_config::parse_with_defaults("", std::path::Path::new("t.toml"))
        .expect("shipped defaults parse");
    state.settings = crate::settings::TuiSettings::tolerant_from(&config);
    state.viewport_dims = (80, 24);

    let a = ResourceId::local(1);
    let history: Vec<u8> = (0..60)
        .flat_map(|n| format!("history-{n:02}\r\n").into_bytes())
        .collect();
    let (kernel, _effects, panes) =
        crate::attach::pane_state::published_test_state(&[(&a, 80, 22, &history)]);
    state.mirror.engine_kernel = kernel;
    state.mirror.panes = panes;
    state.mirror.workspace = ArrangementDoc::single(a.clone());
    state.mirror.focused_resource = Some(a.clone());
    state.layout_read_complete = true;
    drop(frames_sent(&mut client, &mut server).await);

    // Paint the live tail once so the renderer has a front to diff against.
    let sidebar = state.sidebar();
    state.repaint_view(&mut out, sidebar, RepaintLevel::Full);
    let tail = String::from_utf8_lossy(&out).into_owned();
    assert!(tail.contains("history-59"), "{tail:?}");
    assert!(!tail.contains("history-10"), "{tail:?}");

    out.clear();
    let wheel_up = InputEvent::Mouse(MouseEvent {
        action: MouseAction::Press,
        button: MouseButton::Four,
        mods: ModSet::empty(),
        x: 40.0,
        y: 8.0,
    });
    state
        .dispatch_batch(&mut client, &mut out, sidebar, vec![wheel_up; 10])
        .await
        .expect("dispatch");

    let painted = String::from_utf8_lossy(&out).into_owned();
    assert!(
        painted.contains("history-10"),
        "ten notches back must show the scrolled-in rows: {painted:?}"
    );
    assert!(
        !out.windows(4).any(|s| s == b"\x1b[2J"),
        "a local scroll must not clear the screen: {painted:?}"
    );
    assert!(
        frames_sent(&mut client, &mut server).await.is_empty(),
        "a scroll the client claims sends the pane nothing"
    );
    drop(client);
    drop(server);
}
