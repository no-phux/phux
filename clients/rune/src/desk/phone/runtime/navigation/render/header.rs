//! Matched header compositions over a populated, real terminal.
use super::*;

#[test]
fn phone_header_allocates_its_inner_width_to_the_current_pane_name() {
    for width in [320.0, 393.0] {
        let (_, mut rt) = fixture();
        detailed_inventory(&mut rt);
        rt.set_viewport((width, 568.0), 1.0);
        rt.update(|d, tx| d.title(tx));
        settle_for_snapshot(&mut rt);
        let capsule = rt.presented("phone-place").unwrap().0;
        let name = rt
            .elements()
            .into_iter()
            .find(|e| e.text.as_deref() == Some("editor"))
            .expect("the focused pane's title");
        let space = phux_design::tokens::SPACE;
        assert!((name.rect[0] - capsule[0] - space.s3).abs() < 0.5);
        assert!((name.rect[2] - (capsule[2] - space.s3 - space.s4)).abs() < 0.5);
        assert_eq!(
            name.visible, name.rect,
            "the title's full allocation is visible"
        );
    }
}

#[test]
fn phone_header_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (size, scale, native) in [
        ((393, 852), 1, false),
        ((320, 568), 1, false),
        // UIKit has already removed the status/home safe areas and keyboard.
        ((393, 480), 2, true),
        ((320, 340), 2, true),
    ] {
        for (appearance, theme) in [
            ("dark", crate::theme::find("phux")),
            ("light", crate::theme::find("phux-light")),
        ] {
            for state in [
                "quiet",
                "waiting",
                "split",
                "keyboard",
                "large-text",
                "empty",
            ] {
                let (client, mut rt) = fixture();
                let view = rt.state.connection.shown.as_ref().unwrap().view.unwrap();
                // Chrome follows the published terminal ground, not merely
                // the Settings theme. Exercise a genuinely light terminal too.
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
                    d.phone.as_mut().unwrap().keyboard = state == "keyboard";
                    let topology = d.connection.topology.as_mut().unwrap();
                    topology.resources[0].title = Some("Review authentication".into());
                    topology.panes[0].title = Some("Review authentication".into());
                    if state == "waiting" {
                        d.activities.apply(
                            &phux_client_runtime::control::Event::AgentMetadata {
                                terminal_id: ResourceId::local(1),
                                value: Some(br#"{"name":"Claude","state":"blocked"}"#.to_vec()),
                            },
                            &[],
                        );
                    }
                    if state == "split" {
                        use phux_client_core::layout::{LayoutNode, SplitDir};
                        d.phone.as_mut().unwrap().tree = Some(LayoutNode::Split {
                            dir: SplitDir::Horizontal,
                            ratio: 0.5,
                            left: Box::new(LayoutNode::Leaf(ResourceId::local(1))),
                            right: Box::new(LayoutNode::Leaf(ResourceId::local(2))),
                        });
                    }
                    if state == "large-text" {
                        d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
                    }
                    if state == "empty" {
                        d.connection.shown = None;
                        d.machines.current = Some("build workstation".into());
                    }
                    d.title(tx);
                    tx.instant();
                });
                settle_for_snapshot(&mut rt);
                assert_eq!(
                    crate::ui::tokens::Tokens::dark_ground(crate::grid::background(
                        &client.acquire_view(view).unwrap()
                    )),
                    appearance == "dark",
                    "the selected composition really uses its named ground"
                );
                let mut renderer = app_renderer(&gpu);
                paint(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    (size.0 * scale, size.1 * scale),
                    &format!("header-{state}-{appearance}-native-{native}-{scale}x"),
                );
                no_terminal_input(&client);
            }
        }
    }
}
