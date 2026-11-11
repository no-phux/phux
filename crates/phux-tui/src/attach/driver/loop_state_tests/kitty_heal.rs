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

/// Focus-in re-sends images sent more than a few seconds ago, not ones
/// just sent; losing focus sends nothing and parks the timer.
#[tokio::test(flavor = "current_thread")]
async fn an_outer_focus_in_re_sends_kitty_images_not_just_sent() {
    use phux_protocol::input::focus::FocusEvent;

    let (mut state, mut client, server, mut out, _) = kitty_image_loop().await;
    let sidebar = state.sidebar();
    for event in [FocusEvent::Lost, FocusEvent::Gained] {
        state
            .dispatch_batch(&mut client, &mut out, sidebar, vec![InputEvent::Focus(event)])
            .await
            .expect("dispatch");
        assert_eq!(kitty_transmits(&out), 0, "{event:?}: {out:?}");
        out.clear();
    }
    state.on_host_focus(&mut out, sidebar, false);
    assert!(state.housekeeping_deadline().is_none(), "unfocused: no timer");
    let away = std::time::Instant::now() + std::time::Duration::from_secs(6);
    state.heal_kitty_images(&mut out, sidebar, away, true);
    assert_eq!(kitty_transmits(&out), 1, "{out:?}");
    drop(client);
    drop(server);
}

/// The housekeeping timer heals an image once its interval has passed, and
/// not before.
#[tokio::test(flavor = "current_thread")]
async fn the_housekeeping_timer_re_sends_images_after_the_interval() {
    let (mut state, client, server, mut out, _) = kitty_image_loop().await;
    let sidebar = state.sidebar();
    let due = state.housekeeping_deadline().expect("a heal is armed");
    assert!(due > tokio::time::Instant::now());
    state.on_housekeeping_due(&mut out, sidebar);
    assert_eq!(kitty_transmits(&out), 0, "not due yet");

    let later = due.into_std() + std::time::Duration::from_millis(1);
    state.heal_kitty_images(&mut out, sidebar, later, false);
    assert_eq!(kitty_transmits(&out), 1, "{out:?}");
    assert!(state.housekeeping_deadline().is_some_and(|next| next > due));
    drop(client);
    drop(server);
}
