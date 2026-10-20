// ---- overlays and the resolver -----------------------------------------------

/// Regression: while an overlay is up the resolver is bypassed, so the
/// leader chord (and the key after it) reach the overlay as literal input.
#[tokio::test]
async fn overlay_active_prefix_key_reaches_overlay_not_resolver() {
    let keys = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut env = Env::new(CtxFixture::default()).with_default_bindings();
    env.fx
        .overlays
        .push(Box::new(crate::render::overlay::RecordingOverlay {
            keys: keys.clone(),
        }));
    let leader = InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::A,
        mods: ModSet::CTRL,
        consumed_mods: ModSet::CTRL,
        composing: false,
        text: None,
        unshifted_codepoint: Some(u32::from(b'a')),
    });
    env.dispatch(vec![leader, press(PhysicalKey::X, Some("x"))])
        .await;
    let received = keys.borrow();
    assert_eq!(received.len(), 2, "{received:?}");
    assert_eq!(received[0].key, PhysicalKey::A);
    assert!(received[0].mods.contains(ModSet::CTRL));
    assert_eq!(received[1].key, PhysicalKey::X);
}

/// With the prefix pending under a passthrough popup (which-key or first-use
/// guidance), the next chord dismisses it AND runs its binding (`C-a d`
/// detaches); Esc dismisses and cancels the prefix, so a following `d` is a
/// plain keystroke.
#[tokio::test]
async fn passthrough_popups_never_eat_the_pending_chord() {
    for (events, onboarding, detaches) in [
        (vec![press(PhysicalKey::D, Some("d"))], false, true),
        (vec![press(PhysicalKey::D, Some("d"))], true, true),
        (
            vec![
                press(PhysicalKey::Escape, None),
                press(PhysicalKey::D, Some("d")),
            ],
            false,
            false,
        ),
    ] {
        let cfg = default_cfg();
        let mut env = Env::new(CtxFixture::default()).with_default_bindings();
        let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
        let resolver = env.resolver.as_mut().expect("resolver");
        assert_eq!(resolver.feed(prefix), phux_config::keybind::Feed::Partial);
        let theme = Theme::default();
        if onboarding {
            env.fx
                .overlays
                .push(Box::new(crate::render::overlay::ToastOverlay::passthrough(
                    super::super::onboarding::ONBOARDING_TITLE,
                    super::super::onboarding::hint_lines(Some(&cfg.keybindings), true),
                    &theme,
                )));
        } else {
            env.fx.overlays.push(Box::new(
                crate::render::overlay::WhichKeyOverlay::from_config(&cfg.keybindings, &theme),
            ));
        }
        let sent = env.dispatch(events).await;
        assert!(!env.fx.overlays.is_active(), "the popup is dismissed");
        assert!(!env.resolver.as_ref().expect("resolver").is_pending());
        assert_eq!(sent.detach, detaches, "onboarding={onboarding}");
    }
}

/// A kitty-mode host reports Caps Lock on the prefix chord (`CSI 97;69u`);
/// the binding must still fire, as it does for the legacy `0x01`.
#[tokio::test]
async fn prefix_chord_matches_with_caps_lock_reported() {
    let cfg = default_cfg();
    let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
    let mut env = Env::new(CtxFixture::default()).with_default_bindings();
    let leader = InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: prefix.key,
        mods: prefix.modifiers | ModSet::CAPS_LOCK,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    });
    let sent = env
        .dispatch(vec![leader, press(PhysicalKey::D, Some("d"))])
        .await;
    assert!(sent.detach, "prefix + d must detach with Caps Lock on");
}

#[tokio::test]
async fn copy_mode_page_scroll_mutates_focused_terminal_viewport() {
    let mut replay = Vec::new();
    for n in 0..10 {
        replay.extend_from_slice(format!("line{n:02}\r\n").as_bytes());
    }
    let mut env = Env::new(CtxFixture::default()).published(&[(&tid(1), 8, 4, &replay)]);
    env.fx.viewport = (8, 4);
    let visible = |env: &mut Env<'_>| -> String {
        let terminal = super::super::pane_state::published_terminal(&env.fx.engine_kernel, &tid(1))
            .expect("published");
        let slot = env.panes.get_mut(&tid(1)).expect("pane");
        (0..6)
            .filter_map(|col| {
                slot.renderer
                    .read_grapheme_string_at(ReplicaWalk::for_test(terminal), 0, col)
                    .expect("read cell")
            })
            .collect()
    };
    let before = visible(&mut env);
    env.fx
        .overlays
        .push(Box::new(crate::render::overlay::CopyModeOverlay::new(
            0, 0, 8, 4,
        )));
    let sent = env.dispatch(vec![press(PhysicalKey::PageUp, None)]).await;
    assert!(sent.repainted, "scrolling copy-mode triggers a repaint");
    assert_ne!(
        before,
        visible(&mut env),
        "the scroll reaches the focused viewport"
    );
}

/// Copy-mode search end to end through the dispatcher: `/needle` Enter
/// searches the pane's own loaded history, scrolls the hit into view, and
/// selects it; `n` wraps to the next hit and `N` goes back.
#[tokio::test]
async fn copy_mode_search_jumps_between_hits_in_scrollback() {
    let mut replay = Vec::new();
    for n in 0..30 {
        let word = if n == 3 || n == 12 { "needle" } else { "line" };
        replay.extend_from_slice(format!("{word}{n:02}\r\n").as_bytes());
    }
    let mut env = Env::new(CtxFixture::default()).published(&[(&tid(1), 10, 5, &replay)]);
    env.fx.viewport = (10, 5);
    let top_line = |env: &mut Env<'_>| -> String {
        let terminal = super::super::pane_state::published_terminal(&env.fx.engine_kernel, &tid(1))
            .expect("published");
        let slot = env.panes.get_mut(&tid(1)).expect("pane");
        (0..8)
            .filter_map(|col| {
                slot.renderer
                    .read_grapheme_string_at(ReplicaWalk::for_test(terminal), 0, col)
                    .expect("read cell")
            })
            .collect()
    };
    env.fx
        .overlays
        .push(Box::new(crate::render::overlay::CopyModeOverlay::new(
            4, 0, 10, 5,
        )));
    let typed = |text: &str| -> Vec<InputEvent> {
        text.chars()
            .map(|ch| press(PhysicalKey::A, Some(&ch.to_string())))
            .collect()
    };

    let mut search = typed("/needle");
    search.push(press(PhysicalKey::Enter, None));
    let sent = env.dispatch(search).await;
    assert!(sent.repainted, "a search moves the cursor: repaint");
    // From the live bottom, forward wraps to the oldest hit (row 3) and
    // centers it: rows 1..=5 show, the hit on row 2.
    assert_eq!(
        env.fx.overlays.copy_search_status().as_deref(),
        Some("/needle 1/2")
    );
    assert_eq!(top_line(&mut env), "line01");
    let sel = env.fx.overlays.copy_selection().expect("copy-mode");
    assert_eq!(
        (sel.start_row, sel.start_col, sel.end_row, sel.end_col),
        (2, 0, 2, 5)
    );

    env.dispatch(vec![press(PhysicalKey::N, Some("n"))]).await;
    assert_eq!(
        env.fx.overlays.copy_search_status().as_deref(),
        Some("/needle 2/2")
    );
    assert_eq!(top_line(&mut env), "line10");

    env.dispatch(vec![InputEvent::Key(KeyEvent {
        mods: ModSet::SHIFT,
        ..key_event(PhysicalKey::N, Some("N"))
    })])
    .await;
    assert_eq!(
        env.fx.overlays.copy_search_status().as_deref(),
        Some("/needle 1/2")
    );

    env.dispatch(typed("/nothing-here")).await;
    env.dispatch(vec![press(PhysicalKey::Enter, None)]).await;
    assert_eq!(
        env.fx.overlays.copy_search_status().as_deref(),
        Some("/nothing-here: no match")
    );
    assert!(env.fx.overlays.is_active(), "a miss keeps copy-mode open");
}

fn key_event(key: PhysicalKey, text: Option<&str>) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(ToOwned::to_owned),
        unshifted_codepoint: None,
    }
}

/// Bracketed paste into a prompt or picker fills its text (controls
/// stripped) without submitting, dismissing, or reaching the pane.
#[tokio::test]
async fn bracketed_paste_populates_modal_text_without_reaching_the_pane() {
    type Case<'a> = (Box<dyn RenderOverlay>, &'a [u8], &'a str, Option<&'a str>);
    use crate::render::overlay::{
        OverlayOutcome, PromptOverlay, RenderOverlay, SelectItem, SelectList,
    };
    let theme = Theme::default();
    let picker = SelectList::new(
        "Sessions",
        vec![
            SelectItem::new("other", bare_action("wrong-choice")),
            SelectItem::new("kitten", bare_action("chosen-session")),
        ],
        &theme,
    );
    let cases: [Case<'_>; 2] = [
        (
            Box::new(PromptOverlay::rename_window("", &theme)),
            b"build\r\n\t jobs\x1b\x08",
            "rename-window",
            Some("build jobs"),
        ),
        (Box::new(picker), b"k\tit\n", "chosen-session", None),
    ];
    for (modal, payload, expected_action, expected_name) in cases {
        let mut env = two_pane_env(&[], (0, 0));
        env.fx.overlays.push(modal);
        let mut framed = b"\x1b[200~".to_vec();
        framed.extend_from_slice(payload);
        framed.extend_from_slice(b"\x1b[201~");
        let events = crate::attach::input::StdinParser::new().feed(&framed);
        let sent = env.dispatch(events).await;
        assert!(
            sent.frames.is_empty(),
            "paste leaked to the pane: {:?}",
            sent.frames
        );
        assert!(
            env.fx.overlays.is_active(),
            "pasted controls must not submit or dismiss"
        );
        let InputEvent::Key(enter) = press(PhysicalKey::Enter, None) else {
            unreachable!()
        };
        let OverlayOutcome::RunAction(action) = env.fx.overlays.handle_key(&enter) else {
            panic!("pasted text must remain until a real Enter");
        };
        assert_eq!(action.action, expected_action);
        if let Some(name) = expected_name {
            assert_eq!(
                action.args.get("name").and_then(toml::Value::as_str),
                Some(name)
            );
        }
    }
}
