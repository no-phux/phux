/// A session showing one pane with a kitty image, painted once.
async fn kitty_image_loop() -> (SessionLoop, Connection, Connection, Vec<u8>, ResourceId) {
    let (mut state, client, server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    state.viewport_dims = (20, 6);
    let a = ResourceId::local(1);
    let image: &[u8] = b"\x1b_Ga=T,f=32,s=1,v=1,i=77,q=2;/wAA/w==\x1b\\";
    let (kernel, _effects, panes) =
        crate::attach::pane_state::published_test_state(&[(&a, 20, 5, image)]);
    state.mirror.engine_kernel = kernel;
    state.mirror.panes = panes;
    state.mirror.workspace = ArrangementDoc::single(a.clone());
    state.mirror.focused_resource = Some(a.clone());
    state.layout_read_complete = true;
    let sidebar = state.sidebar();
    state.repaint_view(&mut out, sidebar, RepaintLevel::Full);
    assert_eq!(kitty_transmits(&out), 1, "the first paint sends the image");
    out.clear();
    (state, client, server, out, a)
}

fn kitty_transmits(bytes: &[u8]) -> usize {
    bytes.windows(6).filter(|w| *w == b"\x1b_Ga=t").count()
}

/// Regaining the outer terminal's focus re-sends the visible images once;
/// losing it sends nothing.
#[tokio::test(flavor = "current_thread")]
async fn an_outer_focus_in_re_sends_kitty_images() {
    use phux_protocol::input::focus::FocusEvent;

    let (mut state, mut client, server, mut out, _) = kitty_image_loop().await;
    let sidebar = state.sidebar();
    for (event, sends) in [(FocusEvent::Lost, 0), (FocusEvent::Gained, 1)] {
        state
            .dispatch_batch(&mut client, &mut out, sidebar, vec![InputEvent::Focus(event)])
            .await
            .expect("dispatch");
        assert_eq!(kitty_transmits(&out), sends, "{event:?}: {out:?}");
        out.clear();
    }
    drop(client);
    drop(server);
}

/// The housekeeping timer heals an image once its interval has passed, and
/// not before.
#[tokio::test(flavor = "current_thread")]
async fn the_housekeeping_timer_re_sends_images_after_the_interval() {
    let (mut state, client, server, mut out, a) = kitty_image_loop().await;
    let sidebar = state.sidebar();
    let due = state.housekeeping_deadline().expect("a heal is armed");
    assert!(due > tokio::time::Instant::now());
    state.on_housekeeping_due(&mut out, sidebar);
    assert_eq!(kitty_transmits(&out), 0, "not due yet");

    state
        .mirror
        .panes
        .get_mut(&a)
        .expect("slot")
        .renderer
        .backdate_kitty_send();
    assert!(state.housekeeping_deadline().is_some_and(|at| at <= tokio::time::Instant::now()));
    state.on_housekeeping_due(&mut out, sidebar);
    assert_eq!(kitty_transmits(&out), 1, "{out:?}");
    assert!(state.housekeeping_deadline().is_some_and(|at| at > tokio::time::Instant::now()));
    drop(client);
    drop(server);
}
