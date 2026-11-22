//! The phone's waiting, failing and empty moments, and its secondary lists,
//! painted through the real renderer: what stands on the stage before a
//! terminal does, the banner over one that stopped, the command list and its
//! no-match state, and the machines sheet.
use std::time::Duration;

use phux_client_runtime::control::Status;

use super::*;

/// Where each case is drawn: the desktop-hosted phone at both widths, and
/// the native host's safe-area viewport at its true 2× scale.
const SIZES: [((u32, u32), u32, bool); 3] = [
    ((393, 852), 1, false),
    ((320, 568), 1, false),
    ((393, 480), 2, true),
];

/// Each case, and whether it belongs on the native host too (machines and
/// the palette's sheet are the same there; the stage's words are not).
const STATES: [(&str, bool); 8] = [
    ("opening", true),
    ("connecting", true),
    ("slow", true),
    ("unreachable", true),
    ("reconnecting", true),
    ("commands", false),
    ("no-match", false),
    ("machines", false),
];

fn arrange(d: &mut Desk, state: &str, tx: &mut rune::Tx) {
    match state {
        "opening" => {
            d.connection.status = Status::Attached;
            d.connection.shown = None;
        }
        "connecting" | "slow" | "unreachable" => {
            // A cold start: nothing laid out, nothing shown yet.
            d.connection.layout = None;
            d.connection.shown = None;
            d.connection.status = if state == "unreachable" {
                Status::Failed
            } else {
                Status::Connecting
            };
            if state == "unreachable" {
                d.connection.error = Some(
                    "100.79.155.27:8788 did not answer within 15s; check that it is up".into(),
                );
            }
            let ago = if state == "connecting" { 0 } else { 5 };
            d.link.down_for(Duration::from_secs(ago));
        }
        "reconnecting" => {
            d.connection.status = Status::Connecting;
            d.link.down_for(Duration::from_secs(5));
        }
        "commands" | "no-match" => {
            d.toggle_palette();
            if state == "no-match" {
                d.palette_query.replace_text("deploy staging");
                d.palette_edited();
            }
        }
        "machines" => d.toggle_machines(),
        _ => unreachable!("{state}"),
    }
    d.title(tx);
    tx.instant();
}

#[test]
fn phone_states_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (size, scale, native) in SIZES {
        for (appearance, theme) in [
            ("dark", crate::theme::find("phux")),
            ("light", crate::theme::find("phux-light")),
        ] {
            for (state, on_native) in STATES {
                if native && !on_native {
                    continue;
                }
                let (client, mut rt) = fixture();
                let colors = if appearance == "light" {
                    "\x1b]10;#202124\x07\x1b]11;#f5f6f8\x07"
                } else {
                    "\x1b]10;#e2e3e8\x07\x1b]11;#141519\x07"
                };
                client
                    .feed(phux_wire_fixtures::output(
                        &ResourceId::local(1),
                        1,
                        colors.as_bytes(),
                    ))
                    .unwrap();
                detailed_inventory(&mut rt);
                rt.set_viewport((size.0 as f32, size.1 as f32), scale as f32);
                rt.set_system_reduced_motion(true);
                rt.update(|d, tx| {
                    if native {
                        d.native = Some(crate::mobile::Platform::test());
                    }
                    d.settings.theme = theme;
                    d.machines.current = Some("studio".into());
                    d.phone.as_mut().unwrap().keyboard = false;
                    arrange(d, state, tx);
                });
                settle_for_snapshot(&mut rt);
                let mut renderer = app_renderer(&gpu);
                paint(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    (size.0 * scale, size.1 * scale),
                    &format!("states-{state}-{appearance}-native-{native}-{scale}x"),
                );
                no_terminal_input(&client);
            }
        }
    }
}

/// The stage's actions never overlap, each is a full-width touch target on
/// the message's measure, and the message sits centred on the stage.
#[test]
fn the_phone_stage_stacks_its_actions_as_full_width_touch_targets() {
    for width in [320.0, 393.0] {
        let (_, mut rt) = fixture();
        detailed_inventory(&mut rt);
        rt.set_viewport((width, 852.0), 1.0);
        rt.update(|d, tx| {
            d.native = Some(crate::mobile::Platform::test());
            d.machines.current = Some("studio".into());
            d.phone.as_mut().unwrap().keyboard = false;
            arrange(d, "unreachable", tx);
        });
        settle_for_snapshot(&mut rt);
        let retry = rt.presented("placeholder-retry").expect("retry").0;
        let elsewhere = rt.presented("placeholder-machines").expect("elsewhere").0;
        let message = rt.presented("placeholder").expect("the message").0;
        for button in [retry, elsewhere] {
            assert!(button[3] >= 44.0, "{width}: {button:?}");
            assert!((button[2] - (message[2] - 48.0)).abs() < 0.5, "{button:?}");
        }
        assert!(
            retry[1] + retry[3] <= elsewhere[1],
            "{retry:?} over {elsewhere:?}"
        );
        let centre = message[0] + message[2] / 2.0;
        assert!((centre - width / 2.0).abs() < 1.0, "{width}: {message:?}");
        let tree = rt.accessibility_json().to_string();
        assert!(tree.contains("Can't reach studio") && tree.contains("Retry"));
        // The failure's mark is named by its word, never colour alone.
        assert!(tree.contains("failed"), "{tree}");
    }
}

/// A phone with a layout but no pane chosen yet says what is coming,
/// instead of a blank stage; a calm wait offers no Retry.
#[test]
fn a_phone_with_no_pane_chosen_says_a_terminal_is_opening() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((393.0, 852.0), 1.0);
    rt.update(|d, tx| arrange(d, "opening", tx));
    settle_for_snapshot(&mut rt);
    assert!(rt.find("placeholder").is_some(), "the stage is not blank");
    assert!(
        rt.accessibility_json()
            .to_string()
            .contains("Opening terminal")
    );
    assert!(rt.find("placeholder-retry").is_none());
    assert!(rt.find("phone-spatial-stage").is_none());
}
