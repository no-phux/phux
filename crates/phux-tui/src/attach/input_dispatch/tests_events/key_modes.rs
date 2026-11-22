// ---- key modes ---------------------------------------------------------------

/// The two-pane env with the shipped bindings layered under `user`.
fn mode_env(user: &str) -> Env<'static> {
    let cfg = phux_config::parse_with_defaults(user, std::path::Path::new("config.toml"))
        .expect("config parses");
    let mut env = two_pane_env(&[], (8, 16));
    env.resolver = Some(phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver"));
    env.keybindings = Some(cfg.keybindings);
    env
}

fn active_mode(env: &Env<'_>) -> Option<String> {
    env.resolver
        .as_ref()
        .and_then(phux_config::keybind::Resolver::active_mode)
        .map(str::to_owned)
}

fn split_ratio(env: &Env<'_>) -> f32 {
    match env.fx.workspace.active_group().expect("window").tree.as_ref() {
        Some(crate::layout::LayoutNode::Split { ratio, .. }) => *ratio,
        other => panic!("expected a root split, got {other:?}"),
    }
}

fn shift_r() -> InputEvent {
    InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::R,
        mods: ModSet::SHIFT,
        consumed_mods: ModSet::SHIFT,
        composing: false,
        text: Some("R".to_owned()),
        unshifted_codepoint: Some(u32::from(b'r')),
    })
}

/// Prefix R enters the shipped resize mode: `l` resizes without the
/// prefix, an unbound key is swallowed (exclusive), Esc leaves, and the
/// same key then reaches the pane again.
#[tokio::test]
async fn the_resize_mode_resizes_swallows_and_leaves_on_esc() {
    let mut env = mode_env("");
    env.dispatch(vec![ctrl_a(), shift_r()]).await;
    assert_eq!(active_mode(&env).as_deref(), Some("resize"));

    let before = split_ratio(&env);
    let sent = env.dispatch(vec![press(PhysicalKey::L, Some("l"))]).await;
    assert!(split_ratio(&env) > before, "l grew the left pane");
    assert!(sent.repainted);

    let sent = env.dispatch(vec![press(PhysicalKey::X, Some("x"))]).await;
    assert!(sent.frames.is_empty(), "x is swallowed: {:?}", sent.frames);
    assert_eq!(active_mode(&env).as_deref(), Some("resize"));

    env.dispatch(vec![press(PhysicalKey::Escape, None)]).await;
    assert_eq!(active_mode(&env), None);
    let sent = env.dispatch(vec![press(PhysicalKey::X, Some("x"))]).await;
    assert!(!sent.frames.is_empty(), "x reaches the pane again");
}

/// A non-exclusive mode passes unbound keys to the pane; `once` leaves
/// after the first of the mode's keys.
#[tokio::test]
async fn a_once_mode_passes_unbound_keys_through_and_leaves_after_one_binding() {
    let mut env = mode_env(
        "[keybindings.modes.grow]\nl = { action = \"resize-pane\", direction = \"right\", amount = 1 }\n\
         [keybindings.prefix-table]\ne = { action = \"enter-mode\", name = \"grow\", once = true }\n",
    );
    env.dispatch(vec![ctrl_a(), press(PhysicalKey::E, Some("e"))])
        .await;
    assert_eq!(active_mode(&env).as_deref(), Some("grow"));
    let sent = env.dispatch(vec![press(PhysicalKey::X, Some("x"))]).await;
    assert!(!sent.frames.is_empty(), "x reaches the pane");
    assert_eq!(active_mode(&env).as_deref(), Some("grow"));

    let before = split_ratio(&env);
    env.dispatch(vec![press(PhysicalKey::L, Some("l"))]).await;
    assert!(split_ratio(&env) > before);
    assert_eq!(active_mode(&env), None, "once: one binding, then out");
}

/// `enter-mode` naming no mode bells and changes nothing; `exit-mode`
/// outside a mode is a quiet no-op.
#[tokio::test]
async fn entering_an_unknown_mode_bells() {
    let mut env = mode_env("");
    let resolved = act("enter-mode", &[("name", "nope".into())]);
    let mut ctx = env.fx.ctx();
    ctx.resolver = env.resolver.as_mut();
    let effects = super::run_action::run_action(&resolved, &mut ctx, env.focused.as_ref(), &env.panes);
    assert!(effects.bell);
    let effects = super::run_action::run_action(
        &act("exit-mode", &[]),
        &mut ctx,
        env.focused.as_ref(),
        &env.panes,
    );
    assert!(!effects.bell);
    drop(ctx);
    assert_eq!(active_mode(&env), None);
}
