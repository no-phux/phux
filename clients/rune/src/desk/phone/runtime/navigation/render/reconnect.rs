//! Connection feedback over retained terminal content, not the cold-start stage.
use super::*;
use phux_client_runtime::control::Status;
use std::time::Duration;

const SIZES: [((u32, u32), u32, bool, bool); 6] = [
    ((393, 852), 1, true, false),
    ((320, 568), 1, true, false),
    ((393, 480), 2, true, true),
    ((320, 340), 2, true, true),
    ((1100, 720), 1, false, false),
    ((640, 360), 1, false, false),
];

fn arrange(rt: &mut Runtime<Desk>, native: bool, phone: bool, large: bool, state: &str) {
    rt.update(|d, tx| {
        if !phone {
            d.phone = None;
            d.interface_size = if large { 140 } else { 100 };
        } else {
            d.phone.as_mut().unwrap().keyboard = false;
            if native {
                d.native = Some(crate::mobile::Platform::test());
            }
            d.set_accessibility(
                false,
                if large {
                    "UICTContentSizeCategoryAccessibilityXXXL"
                } else {
                    "UICTContentSizeCategoryL"
                },
            );
        }
        d.machines.current = Some("studio — build and review".into());
        d.connection.status = Status::Connecting;
        d.link.down_for(Duration::from_secs(5));
        match state {
            "reconnecting" => {}
            "long" => {
                d.link.ended = Some(
                    "The connection was interrupted. Check that Tailscale is on and the computer is awake."
                        .into(),
                );
            }
            "pair" => {
                d.connection.status = Status::Failed;
                d.connection.error = Some("studio refused the pairing token (unauthorized)".into());
            }
            "update" => {
                d.connection.status = Status::Failed;
                d.failed_with(
                    "VersionIncompatible: the server was upgraded; update the phux app/client",
                );
            }
            _ => unreachable!("{state}"),
        }
        tx.instant();
    });
}

#[test]
fn reconnect_feedback_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (size, scale, phone, native) in SIZES {
        for large in [false, true] {
            for (appearance, theme) in [
                ("dark", crate::theme::find("phux")),
                ("light", crate::theme::find("phux-light")),
            ] {
                for state in ["reconnecting", "long", "pair", "update"] {
                    let (client, mut rt) = fixture();
                    detailed_inventory(&mut rt);
                    rt.set_viewport((size.0 as f32, size.1 as f32), scale as f32);
                    rt.set_system_reduced_motion(true);
                    rt.update(|d, _| d.settings.theme = theme);
                    arrange(&mut rt, native, phone, large, state);
                    settle_for_snapshot(&mut rt);
                    assert!(rt.find("reconnect-banner").is_some());
                    let mut renderer = app_renderer(&gpu);
                    let name = format!(
                        "reconnect-{state}-{appearance}-large-{large}-phone-{phone}-native-{native}-{scale}x"
                    );
                    for phase in ["top", "bottom", "focused"] {
                        if !phone && phase != "top" {
                            continue;
                        }
                        reveal(&mut rt, phase);
                        paint(
                            &gpu,
                            &mut renderer,
                            &mut rt,
                            (size.0 * scale, size.1 * scale),
                            &format!("{name}-{phase}"),
                        );
                    }
                    no_terminal_events(&client);
                }
            }
        }
    }
}

fn reveal(rt: &mut Runtime<Desk>, phase: &str) {
    if phase == "bottom" {
        rt.update(|_, tx| tx.scroll_to_instant("reconnect-content", 0.0, 100_000.0));
    } else if phase == "focused" {
        rt.update(|_, tx| tx.scroll_to_instant("reconnect-content", 0.0, 0.0));
        keyboard_focus(rt, "banner-retry");
    }
    settle_for_snapshot(rt);
}

fn keyboard_focus(rt: &mut Runtime<Desk>, key: &str) {
    // Tx::focus sets focus, not a reveal request. Traverse back then forward:
    // real Tab navigation both sets keyboard focus and reveals the target.
    rt.update(|_, tx| tx.focus(key, false));
    settle_for_snapshot(rt);
    rt.key(rune::KeyEvent {
        key: rune::Key::Tab,
        mods: rune::Mods {
            shift: true,
            ..rune::Mods::default()
        },
    });
    settle_for_snapshot(rt);
    rt.key(crate::keys::plain(rune::Key::Tab));
    settle_for_snapshot(rt);
}

#[test]
fn reconnect_feedback_keeps_copy_and_recovery_reachable_above_the_accessory() {
    for (size, scale, phone, native) in SIZES {
        if !phone {
            continue;
        }
        for large in [false, true] {
            for state in ["reconnecting", "long", "pair", "update"] {
                let (client, mut rt) = fixture();
                detailed_inventory(&mut rt);
                rt.set_viewport((size.0 as f32, size.1 as f32), scale as f32);
                rt.set_system_reduced_motion(true);
                arrange(&mut rt, native, phone, large, state);
                settle_for_snapshot(&mut rt);
                check_recovery_actions(&mut rt, native, state);
                no_terminal_events(&client);
            }
        }
    }
}

fn check_recovery_actions(rt: &mut Runtime<Desk>, native: bool, state: &str) {
    let banner = crate::ui::element(rt, "reconnect-banner");
    let accessory = crate::ui::element(rt, "phone-accessory");
    assert!(
        banner.rect[1] + banner.rect[3] <= accessory.rect[1],
        "{banner:?} behind {accessory:?}"
    );
    let pane = crate::ui::element(rt, "phone-spatial-stage").rect;
    let mut actions = vec!["banner-retry"];
    if native && state == "pair" {
        actions.push("banner-pair");
    }
    if !native && state == "update" {
        actions.push("banner-update");
    }
    for key in actions {
        rt.update(|_, tx| tx.scroll_to_instant("reconnect-content", 0.0, 0.0));
        keyboard_focus(rt, key);
        assert_complete_action(rt, key);
        assert_eq!(crate::ui::element(rt, "phone-spatial-stage").rect, pane);
    }
}

fn assert_complete_action(rt: &Runtime<Desk>, key: &str) {
    let action = crate::ui::element(rt, key);
    assert!(action.focused && action.focus_visible, "{action:?}");
    assert!(
        action.rect[2] >= 44.0 && action.rect[3] >= 44.0,
        "{action:?}"
    );
    for axis in [0, 1] {
        assert!(
            (action.rect[axis + 2] - action.visible[axis + 2]).abs() < 0.5,
            "clipped {action:?}"
        );
    }
    let viewport = crate::ui::element(rt, "reconnect-content").visible;
    let ring = phux_design::tokens::SPACE.focus_ring;
    let clearance = ring.offset + ring.width;
    assert!(
        action.rect[1] - clearance >= viewport[1]
            && action.rect[1] + action.rect[3] + clearance <= viewport[1] + viewport[3],
        "focus ring must remain visible: {action:?}, {viewport:?}"
    );
}

#[test]
fn reconnect_feedback_pair_touch_opens_only_the_native_connection_sheet() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 340.0), 2.0);
    arrange(&mut rt, true, true, true, "pair");
    settle_for_snapshot(&mut rt);
    keyboard_focus(&mut rt, "banner-pair");
    tap(&mut rt, "banner-pair", 3.0);
    assert!(rt.state.native.as_ref().unwrap().connection_requested);
    assert!(rt.state.unpaired(), "the action does not manufacture trust");
    no_terminal_events(&client);
}

#[test]
fn reconnect_feedback_touch_scroll_reveals_retry_without_moving_the_terminal() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 340.0), 2.0);
    rt.set_system_reduced_motion(true);
    arrange(&mut rt, true, true, true, "long");
    settle_for_snapshot(&mut rt);
    let pane = crate::ui::element(&rt, "phone-spatial-stage").rect;
    let view = rt.state.connection.shown.as_ref().unwrap().view.unwrap();
    let offset = client.acquire_view(view).unwrap().scrollbar.offset;
    for swipe in 0..10 {
        let at = 3.0 + f64::from(swipe);
        rt.touch(1, TouchPhase::Started, 160.0, 240.0, at);
        assert!(rt.state.phone.as_ref().unwrap().touch.is_none());
        rt.touch(1, TouchPhase::Moved, 160.0, 80.0, at + 0.1);
        rt.touch(1, TouchPhase::Ended, 160.0, 80.0, at + 0.2);
        settle_for_snapshot(&mut rt);
    }
    let action = crate::ui::element(&rt, "banner-retry");
    assert!(
        (action.visible[3] - action.rect[3]).abs() < 0.5,
        "touch scrolling must reveal the complete action: {action:?}"
    );
    assert_eq!(crate::ui::element(&rt, "phone-spatial-stage").rect, pane);
    assert_eq!(client.acquire_view(view).unwrap().scrollbar.offset, offset);
    assert!(!rt.state.native.as_ref().unwrap().connection_requested);
    no_terminal_events(&client);
}

fn no_terminal_events(client: &phux_client_runtime::Client) {
    for bytes in client.take_outbound() {
        assert!(
            !matches!(
                FrameKind::decode(&bytes).expect("decode outbound").0,
                FrameKind::InputKey { .. }
                    | FrameKind::InputPaste { .. }
                    | FrameKind::InputMouse { .. }
            ),
            "feedback input must not reach the terminal"
        );
    }
}

#[test]
fn reconnect_feedback_retry_touch_runs_the_existing_recovery_handler() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 340.0), 2.0);
    rt.set_system_reduced_motion(true);
    arrange(&mut rt, true, true, true, "update");
    settle_for_snapshot(&mut rt);
    assert!(rt.state.link.incompatible.is_some());
    keyboard_focus(&mut rt, "banner-retry");
    tap(&mut rt, "banner-retry", 3.0);
    assert!(rt.state.link.incompatible.is_none());
    assert!(!rt.state.native.as_ref().unwrap().connection_requested);
    no_terminal_events(&client);
}

#[test]
fn reconnect_feedback_padding_shields_a_nonoverflowing_card_from_the_terminal() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((393.0, 480.0), 2.0);
    rt.set_system_reduced_motion(true);
    arrange(&mut rt, true, true, false, "reconnecting");
    settle_for_snapshot(&mut rt);
    let view = rt.state.connection.shown.as_ref().unwrap().view.unwrap();
    let selection_before = client.engine().unwrap().view_selection_text(view);
    // The first view query can catch up a deferred publication. The no-input
    // diagnostic advanced 2→3 here, then padding input stayed 3→3. Snapshot
    // immediately before the gesture, not before this observer-side catch-up.
    settle_for_snapshot(&mut rt);
    let before = client.acquire_view(view).unwrap();
    let card = crate::ui::element(&rt, "reconnect-banner").rect;
    let (x, y) = (card[0] + 2.0, card[1] + 2.0);
    rt.touch(1, TouchPhase::Started, x, y, 3.0);
    assert!(rt.state.phone.as_ref().unwrap().touch.is_none());
    rt.touch(1, TouchPhase::Ended, x, y, 3.1);
    settle_for_snapshot(&mut rt);
    rt.touch(1, TouchPhase::Started, x, y + 70.0, 4.0);
    assert!(rt.state.phone.as_ref().unwrap().touch.is_none());
    rt.touch(1, TouchPhase::Moved, x, y + 10.0, 4.1);
    rt.touch(1, TouchPhase::Ended, x, y + 10.0, 4.2);
    settle_for_snapshot(&mut rt);
    let after = client.acquire_view(view).unwrap();
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.scrollbar.offset, before.scrollbar.offset);
    assert_eq!(after.text(), before.text());
    let selection_after = client.engine().unwrap().view_selection_text(view);
    assert_eq!(
        format!("{selection_before:?}"),
        format!("{selection_after:?}")
    );
    assert!(!rt.state.phone.as_ref().unwrap().keyboard);
    assert!(!rt.state.native.as_ref().unwrap().connection_requested);
    no_terminal_events(&client);
}
