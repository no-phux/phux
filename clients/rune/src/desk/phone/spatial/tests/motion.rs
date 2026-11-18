//! Staged real-runtime motion evidence, unchanged between before/after runs.
//! No image expectations: only layout/input invariants shared by both versions.
use super::*;
use rune::Runtime;

const DT: f64 = 1.0 / 60.0;

#[derive(Clone, Copy)]
struct Case {
    size: (u32, u32),
    scale: u32,
    native: bool,
    keyboard: bool,
    reduced: bool,
    gesture: &'static str,
}

impl Case {
    fn name(self, appearance: &str) -> String {
        format!(
            "pane-motion-{}-{appearance}-{}x{}-{}x-native-{}-keyboard-{}-reduced-{}",
            self.gesture,
            self.size.0,
            self.size.1,
            self.scale,
            self.native,
            self.keyboard,
            self.reduced
        )
    }

    fn points(self) -> [[f32; 2]; 2] {
        match self.gesture {
            "vertical" => [[-12.0, -24.0], [-52.0, -80.0]],
            "boundary" => [[24.0, 12.0], [80.0, 52.0]],
            "reversal" => [[-24.0, -12.0], [60.0, -38.0]],
            _ => [[-24.0, -12.0], [-80.0, -52.0]],
        }
    }
}

/// Integer frames, not wall time. A touch event is dispatched at the start of
/// its frame; painting and exporting never advance the runtime clock.
struct Capture<'a> {
    gpu: &'a rune::render::Gpu,
    renderer: rune::render::Renderer,
    case: Case,
    name: String,
    frame: u32,
    origin: (f32, f32),
}

impl Capture<'_> {
    fn advance(&mut self, rt: &mut Runtime<Desk>, frames: u32) {
        for _ in 0..frames {
            rt.frame(DT);
            self.frame += 1;
        }
    }

    fn event(&mut self, rt: &mut Runtime<Desk>, phase: rune::TouchPhase, delta: [f32; 2]) {
        rt.touch(
            1,
            phase,
            self.origin.0 + delta[0],
            self.origin.1 + delta[1],
            f64::from(self.frame) / 60.0,
        );
        self.advance(rt, 1);
    }

    fn save(&mut self, rt: &mut Runtime<Desk>, stage: &str) {
        let size = (
            self.case.size.0 * self.case.scale,
            self.case.size.1 * self.case.scale,
        );
        let pixels =
            crate::ui::offscreen_as_app_with_renderer(self.gpu, &mut self.renderer, rt, size);
        assert_eq!(pixels.len(), (size.0 * size.1 * 4) as usize);
        let rgba = pixels.as_chunks::<4>().0;
        assert!(rgba.iter().any(|pixel| *pixel != rgba[0]), "flat {stage}");
        let Some(directory) = std::env::var_os("RUNE_NAVIGATION_SNAP") else {
            return;
        };
        let path = std::path::PathBuf::from(directory).join(format!("{}-{stage}.png", self.name));
        crate::test_support::visual::write(&path, rt, size, &pixels, &self.name);
        let elements: Vec<_> = [
            "phone-canvas",
            "phone-spatial-stage",
            "phone-map-focus",
            "phone-navigation",
            "phone-navigation-toolbar",
            "phone-minimap",
            "phone-pane-picker",
            "phone-keyboard",
        ]
        .into_iter()
        .filter_map(|key| {
            rt.elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some(key))
                .map(|e| {
                    serde_json::json!({
                        "key": key, "rect": e.rect, "target": e.target,
                        "visible": e.visible, "scale": e.scale,
                    })
                })
        })
        .collect();
        let metadata = serde_json::json!({
            "schema": 1, "scenario": self.name, "stage": stage,
            "frame": self.frame, "elapsed_ms": f64::from(self.frame) * 1000.0 / 60.0,
            "fixed_dt_seconds": DT, "touch_origin": self.origin,
            "touch_deltas": self.case.points(),
            "release_frame": 4, "after_release_ms":
                (f64::from(self.frame) - 4.0) * 1000.0 / 60.0,
            "focus": focus(rt).to_string(), "animating": rt.animating,
            "travel": rt.state.phone.as_ref().unwrap().travel,
            "elements": elements,
        });
        std::fs::write(
            path.with_extension("motion.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();
    }
}

fn run(gpu: &rune::render::Gpu, case: Case, appearance: &str) {
    let (client, mut rt) = fixture(true);
    let before = stored(&client);
    let colors = if appearance == "light" {
        "\x1b]10;#202124\x07\x1b]11;#f5f6f8\x07"
    } else {
        "\x1b]10;#e2e3e8\x07\x1b]11;#141519\x07"
    };
    for n in [1, 2, 3, 5] {
        output(&client, n, 1, colors.as_bytes());
    }
    poll(&mut rt);
    rt.set_viewport((case.size.0 as f32, case.size.1 as f32), case.scale as f32);
    rt.set_system_reduced_motion(case.reduced);
    rt.set_srgb_blend(true);
    rt.update(|d, _| {
        d.settings.theme = crate::theme::find(if appearance == "light" {
            "phux-light"
        } else {
            "phux"
        });
        d.phone.as_mut().unwrap().keyboard = case.keyboard;
        if case.native {
            d.native = Some(crate::mobile::Platform::test());
        }
    });
    // Only initial setup settles. Every subsequent capture is a current frame
    // on the explicit timeline below, including the release spring.
    settle(&mut rt);
    let view = rt.state.connection.shown.as_ref().unwrap().view.unwrap();
    assert_eq!(
        crate::ui::tokens::Tokens::dark_ground(crate::grid::background(
            &client.acquire_view(view).unwrap()
        )),
        appearance == "dark",
        "terminal and chrome must genuinely use the named appearance"
    );
    let origin = rt.find("phone-navigation").expect("dedicated Move target");
    let mut renderer = rune::render::Renderer::new(
        &gpu.device,
        rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix(),
    );
    renderer.set_srgb_blend(true);
    let mut capture = Capture {
        gpu,
        renderer,
        case,
        name: case.name(appearance),
        frame: 0,
        origin,
    };
    capture.save(&mut rt, "idle");
    capture.event(&mut rt, Started, [0.0; 2]);
    capture.save(&mut rt, "start");
    let [first, last] = case.points();
    capture.event(&mut rt, Moved, first);
    capture.save(&mut rt, "first-axis");
    capture.event(&mut rt, Moved, last);
    capture.save(&mut rt, "mid-drag");
    assert_eq!(focus(&rt), id(1), "no focus commit before release");
    let release = if case.gesture == "cancel" {
        Cancelled
    } else {
        Ended
    };
    capture.event(&mut rt, release, last);
    capture.save(&mut rt, "release");
    capture.advance(&mut rt, 6);
    capture.save(&mut rt, "release-plus-100ms");
    capture.advance(&mut rt, 90);
    capture.save(&mut rt, "settled");
    assert_eq!(
        stored(&client),
        before,
        "navigation must not rewrite stored layout"
    );
    no_input(&client);
}

#[test]
fn phone_pane_motion_visual_review_frames() {
    if let Some(directory) = std::env::var_os("RUNE_NAVIGATION_SNAP") {
        std::fs::create_dir_all(directory).unwrap();
    }
    let gpu = rune::render::Gpu::headless();
    let small = Case {
        size: (320, 568),
        scale: 1,
        native: false,
        keyboard: false,
        reduced: false,
        gesture: "horizontal",
    };
    let main = [
        small,
        Case {
            size: (393, 852),
            ..small
        },
        Case {
            size: (393, 480),
            scale: 2,
            native: true,
            ..small
        },
        Case {
            size: (320, 340),
            scale: 2,
            native: true,
            ..small
        },
    ];
    for appearance in ["dark", "light"] {
        for case in main {
            run(&gpu, case, appearance);
        }
        for gesture in ["vertical", "cancel", "boundary", "reversal"] {
            run(&gpu, Case { gesture, ..small }, appearance);
        }
        run(
            &gpu,
            Case {
                keyboard: true,
                ..small
            },
            appearance,
        );
        run(
            &gpu,
            Case {
                reduced: true,
                ..small
            },
            appearance,
        );
    }
}
