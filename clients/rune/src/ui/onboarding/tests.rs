//! Every step through the real window, and fixture renders of each.

use rune::accesskit::Action;
use rune::{Env, Key, KeyEvent, Mods, Runtime};

use super::*;
use crate::onboarding::{AgentSkill, KEEP_ALIVE, Permissions};

const SIZE: (f32, f32) = (1100.0, 720.0);
const SCALE: f32 = 2.0;

fn skill(agent: &str, state: &str) -> AgentSkill {
    AgentSkill {
        agent: agent.to_owned(),
        detected: true,
        skill: Some(state.to_owned()),
        ..AgentSkill::default()
    }
}

/// The real window on `step`, as a first run with Claude Code and Codex
/// found and one permission granted would show it.
fn window(step: Step) -> Runtime<Desk> {
    let mut desk = Desk::settings_test_fixture();
    desk.resources.images = Some(rune::ImageCache::new(std::sync::Arc::new(|| {})));
    desk.settings_open = false;
    desk.settings.theme = theme::find("midnight");
    let mut rt = Runtime::new(desk, crate::ui::view, SIZE, Env::new(true, SCALE));
    rt.on_key = Some(Desk::key);
    rt.update(|d, tx| {
        d.open_onboarding();
        // The offline fixture has no CLI: drop what that answered.
        d.pump_onboarding(tx);
        let ob = &mut d.onboarding;
        ob.skills = Probe::Ready(vec![
            skill("claude", "installed"),
            skill("codex", "missing"),
        ]);
        ob.permissions = Probe::Ready(Permissions {
            responsible_path: Some("/Applications/phux Server.app".into()),
            services: vec![(Service::FullDiskAccess, Grant::Granted)],
        });
        ob.bundle = Some("/Applications/phux Server.app".into());
        ob.step = step;
    });
    settle(&mut rt);
    rt
}

fn settle(rt: &mut Runtime<Desk>) {
    for t in [0.0, 0.5, 1.0, 2.0] {
        rt.frame(t);
    }
}

fn has(rt: &Runtime<Desk>, key: &str) -> bool {
    rt.find(key).is_some()
}

/// `RUNE_ONBOARDING_SNAP=DIR` writes each frame as `DIR/NAME.png`.
fn snap(rt: &mut Runtime<Desk>, name: &str) {
    let Some(dir) = std::env::var_os("RUNE_ONBOARDING_SNAP") else {
        return;
    };
    let (w, h) = ((SIZE.0 * SCALE) as u32, (SIZE.1 * SCALE) as u32);
    let gpu = rune::render::Gpu::headless();
    let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
    let target = rune::render::Offscreen::new(&gpu.device, w, h);
    renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
    renderer.prepare(
        &gpu.device,
        &gpu.queue,
        &mut rt.text.atlas,
        &rt.instances,
        (w, h),
    );
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    renderer.draw(&mut enc, &target.view, [0.0; 4]);
    target.copy(&mut enc);
    renderer.submit(&gpu.queue, enc);
    let pixels = target.read(&gpu.device);
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    std::fs::create_dir_all(&dir).unwrap();
    rune::render::write_png(&path, w, h, &pixels).unwrap();
}

#[test]
fn every_step_renders_its_parts() {
    let mut rt = window(Step::Welcome);
    assert!(has(&rt, "onboarding-continue") && has(&rt, "onboarding-skip"));
    assert!(rt.state.fox_image().is_some(), "the brand fox, not a glyph");
    assert!(!has(&rt, "onboarding-back"), "nothing to go back to");
    snap(&mut rt, "1-welcome");

    let mut rt = window(Step::Look);
    for key in [
        "look-auto",
        "look-light",
        "look-dark",
        "look-sidebar",
        "look-theme-midnight",
    ] {
        assert!(has(&rt, key), "{key}");
    }
    assert!(
        !has(&rt, "look-theme-catppuccin-latte"),
        "dark themes only on Dark"
    );
    snap(&mut rt, "2-look");

    let mut rt = window(Step::Agents);
    assert!(has(&rt, "onboarding-skill") && has(&rt, "onboarding-install"));
    assert!(has(&rt, "onboarding-shims") && has(&rt, "onboarding-shim"));
    assert_eq!(
        has(&rt, "onboarding-permissions"),
        PERMISSIONS,
        "macOS only"
    );
    snap(&mut rt, "3-agents");
    if PERMISSIONS {
        // One frame per runtime: a snapshot takes the glyph atlas's uploads.
        let mut rt = window(Step::Agents);
        assert!(rt.accessibility_action("onboarding-grant", Action::Click, None));
        settle(&mut rt);
        assert!(!has(&rt, "grant-full_disk_access"), "granted shows Granted");
        assert!(has(&rt, "grant-screen_recording") && has(&rt, "grant-accessibility"));
        assert!(has(&rt, "grant-drag"), "the server's app to drag in");
        let badge = rt.state.badge_image().expect("the app badge").clone();
        assert!(badge.wait().is_ok(), "the pinned app icon decodes");
        settle(&mut rt);
        snap(&mut rt, "3b-grant-sheet");
    }

    let mut rt = window(Step::Migrate);
    for key in [
        "migrate-claude",
        "migrate-codex",
        "migrate-skip",
        "migrate-copy",
    ] {
        assert!(has(&rt, key), "{key}");
    }
    assert!(
        !has(&rt, "migrate-opencode"),
        "only agents that are installed"
    );
    snap(&mut rt, "4-migrate");
    let mut rt = window(Step::Migrate);
    rt.update(|d, _| d.onboarding.migrate = Migrate::Running("claude".into(), None));
    settle(&mut rt);
    assert!(has(&rt, "migrate-status") && has(&rt, "migrate-cancel"));
    assert!(has(&rt, "terminal"), "the agent's pane beside the step");
    assert_eq!(rt.state.modal(), None, "typing reaches the agent");
    snap(&mut rt, "4b-migrating");

    let mut rt = window(Step::Done);
    for key in [
        "done-key-0",
        "done-key-1",
        "done-key-2",
        "onboarding-start",
        "pair-show",
    ] {
        assert!(has(&rt, key), "{key}");
    }
    assert_eq!(has(&rt, "onboarding-keep-alive"), KEEP_ALIVE, "Linux only");
    snap(&mut rt, "5-done");
    // A fresh window: a second snap's renderer misses glyphs the first uploaded.
    let mut rt = window(Step::Done);
    rt.update(|d, _| d.pairing_fixture());
    for t in [3.0, 4.0, 5.0] {
        rt.frame(t);
    }
    rt.update(|_, tx| tx.scroll_into_view("pair-new"));
    for t in [6.0, 7.0] {
        rt.frame(t);
    }
    assert!(has(&rt, "pair-qr") && !has(&rt, "pair-devices"));
    snap(&mut rt, "5-done-pair");
    if KEEP_ALIVE {
        let mut rt = window(Step::Done);
        assert!(rt.state.onboarding.keep_alive, "on by default");
        assert!(rt.accessibility_action("onboarding-keep-alive", Action::Click, None));
        assert!(!rt.state.onboarding.keep_alive, "the switch turns it off");
    }
}

#[test]
fn picks_apply_to_the_window_and_keys_move_on() {
    let mut rt = window(Step::Look);
    assert!(rt.accessibility_action("look-light", Action::Click, None));
    rt.frame(3.0);
    assert!(
        rt.state.settings.theme.is_some_and(|t| !t.dark),
        "a light theme, live"
    );
    assert!(rt.accessibility_action("look-auto", Action::Click, None));
    rt.frame(3.5);
    assert_eq!(rt.state.settings.theme, None, "Auto follows the system");
    rt.key(KeyEvent {
        key: Key::Enter,
        mods: Mods::default(),
    });
    rt.frame(4.0);
    assert_eq!(rt.state.onboarding.step, Step::Agents);
    rt.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    rt.frame(4.5);
    assert_eq!(rt.state.onboarding.step, Step::Look);
}

#[test]
fn start_using_phux_closes_it() {
    let mut rt = window(Step::Done);
    assert!(rt.accessibility_action("onboarding-start", Action::Click, None));
    settle(&mut rt);
    assert!(!rt.state.onboarding.open);
    assert!(!has(&rt, "onboarding"));
}

#[test]
fn the_glass_slider_steps_and_clamps() {
    let mut rt = window(Step::Look);
    assert_eq!(
        has(&rt, "look-glass"),
        cfg!(target_os = "macos"),
        "macOS only"
    );
    rt.state.set_glass(250);
    assert_eq!(rt.state.settings.glass, 100, "clamped");
    rt.state.set_glass(0);
    if !cfg!(target_os = "macos") {
        assert_eq!(
            rt.state.glass(),
            0.0,
            "no glass without a transparent window"
        );
        return;
    }
    rt.frame(0.1);
    assert!(rt.accessibility_action("look-glass", Action::Focus, None));
    for key in [Key::Right, Key::Right] {
        rt.key(KeyEvent {
            key,
            mods: Mods::default(),
        });
        rt.frame(0.2);
    }
    assert_eq!(rt.state.settings.glass, 10, "arrows step by 5");
    assert!((rt.state.glass() - 0.10).abs() < 1e-6);
    assert_eq!(
        rt.state.onboarding.step,
        Step::Look,
        "arrows don't change step"
    );
    rt.key(KeyEvent {
        key: Key::End,
        mods: Mods::default(),
    });
    rt.frame(0.3);
    assert_eq!(rt.state.settings.glass, 100);
    snap(&mut rt, "2b-look-glass");
}

#[test]
fn skill_shows_installed_with_no_agent_found() {
    let mut rt = window(Step::Agents);
    rt.update(|d, _| {
        let undetected = |agent, state| AgentSkill {
            detected: false,
            ..skill(agent, state)
        };
        d.onboarding.skills = Probe::Ready(vec![
            undetected("claude", "missing"),
            undetected("codex", "up_to_date"),
        ]);
    });
    settle(&mut rt);
    assert!(
        !has(&rt, "onboarding-install"),
        "the shared skill is in place"
    );
}
