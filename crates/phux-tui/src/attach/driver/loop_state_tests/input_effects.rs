/// A prefix press paints its chip on the bar in the same batch, before
/// which-key's delay and without a frame to the pane; the key that
/// completes the chord takes it down again.
#[tokio::test(flavor = "current_thread")]
async fn the_prefix_press_shows_its_chip_on_the_bar_until_the_chord_completes() {
    use phux_config::keybind::parse_chord;

    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let config = phux_config::parse_with_defaults("", std::path::Path::new("t.toml"))
        .expect("shipped defaults parse");
    state.settings = crate::settings::TuiSettings::tolerant_from(&config);
    state.viewport_dims = (80, 24);

    let a = ResourceId::local(1);
    let (kernel, _effects, panes) =
        crate::attach::pane_state::published_test_state(&[(&a, 80, 22, b"")]);
    state.mirror.engine_kernel = kernel;
    state.mirror.panes = panes;
    state.mirror.workspace = ArrangementDoc::single(a.clone());
    state.mirror.focused_resource = Some(a.clone());
    state.layout_read_complete = true;
    drop(frames_sent(&mut client, &mut server).await);
    let sidebar = state.sidebar();
    state.repaint_view(&mut out, sidebar, RepaintLevel::Full);

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
    out.clear();
    state
        .dispatch_batch(&mut client, &mut out, sidebar, vec![press("C-a")])
        .await
        .expect("dispatch");
    let painted = String::from_utf8_lossy(&out).into_owned();
    assert!(
        painted.contains(" C-a "),
        "the prefix press paints the chip: {painted:?}"
    );
    assert!(
        frames_sent(&mut client, &mut server).await.is_empty(),
        "the prefix itself reaches no pane"
    );

    // `C-a d` would detach; `C-a z` zooms the one pane, a layout change
    // that repaints the chrome, so the chip must be gone from that paint.
    out.clear();
    state
        .dispatch_batch(&mut client, &mut out, sidebar, vec![press("z")])
        .await
        .expect("dispatch");
    let painted = String::from_utf8_lossy(&out).into_owned();
    assert!(
        !painted.contains(" C-a "),
        "the completed chord clears the chip: {painted:?}"
    );
    assert!(
        state
            .settings
            .status_bar
            .as_ref()
            .is_some_and(|bar| !bar.prefix_pending_for_test()),
        "the painter no longer holds the chip"
    );
    drop(client);
    drop(server);
}

/// ADR-0172: the one job arm toasts a result and, for a job that changed
/// the session graph, asks for a fresh host inventory; a quiet finish shows
/// and sends nothing.
#[tokio::test(flavor = "current_thread")]
async fn a_finished_job_toasts_and_refreshes_only_what_it_asks() {
    use crate::attach::jobs::Toast;
    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::with(&[ServerFeature::HostSessions])).await;
    frames_sent(&mut client, &mut server).await;

    state
        .on_job_done(&mut client, &mut out, None, JobDone::quiet())
        .await
        .unwrap();
    assert!(!state.overlays.is_active(), "a quiet finish shows nothing");
    assert!(frames_sent(&mut client, &mut server).await.is_empty());

    // The job's running notice is up; the result takes its place.
    state
        .overlays
        .push(Box::new(crate::render::overlay::ToastOverlay::passthrough(
            "Organize",
            vec!["Organizing… the result will appear here.".to_owned()],
            &state.settings.theme,
        )));
    let done = JobDone::toast(Toast::new("Organize", "2 panes moved")).refreshing_inventory();
    state
        .on_job_done(&mut client, &mut out, None, done)
        .await
        .unwrap();
    assert!(state.overlays.is_active(), "the result toasts");
    assert_eq!(
        state.overlays.depth(),
        1,
        "the result replaced the running notice instead of stacking on it"
    );
    assert!(
        !state.overlays.top_is_passthrough(),
        "the result holds the keyboard like any toast"
    );
    assert!(state.session_picker_dirty && state.peers.chrome_dirty);
    let sent = frames_sent(&mut client, &mut server).await;
    assert!(
        sent.iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::GetState { .. },
                ..
            }
        )),
        "the inventory refresh was requested: {sent:?}"
    );
}

/// A key press for `spec`, as the attach parser decodes it.
fn chord_press(spec: &str) -> InputEvent {
    let chord = phux_config::keybind::parse_chord(spec).expect("chord");
    InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: chord.key,
        mods: chord.modifiers,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    })
}

/// A bootstrapped loop showing panes 1 and 2 side by side, focus on 1, plus
/// any `extra` published slots outside the layout.
async fn side_by_side_loop(
    extra: &[(&ResourceId, u16, u16, &[u8])],
) -> (SessionLoop, Connection, Connection, Vec<u8>) {
    use crate::layout::{LayoutNode, LayoutState, SplitDir, split_at};

    let (mut state, mut client, mut server, out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let config = phux_config::parse_with_defaults("", std::path::Path::new("t.toml"))
        .expect("shipped defaults parse");
    state.settings = crate::settings::TuiSettings::tolerant_from(&config);
    state.viewport_dims = (80, 24);

    let (a, b) = (ResourceId::local(1), ResourceId::local(2));
    let mut seeds: Vec<(&ResourceId, u16, u16, &[u8])> = vec![
        (&a, 30, 20, b"left pane\r\n"),
        (&b, 30, 20, b"right pane\r\n"),
    ];
    seeds.extend_from_slice(extra);
    let (kernel, _effects, panes) = crate::attach::pane_state::published_test_state(&seeds);
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
    state.mirror.focused_resource = Some(a);
    state.layout_read_complete = true;
    drop(frames_sent(&mut client, &mut server).await);
    (state, client, server, out)
}
