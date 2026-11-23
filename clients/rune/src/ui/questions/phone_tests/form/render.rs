//! 152 explicitly inventoried form frames; no production geometry expectations.
use super::*;

struct Capture<'a> {
    gpu: &'a rune::render::Gpu,
    renderer: rune::render::Renderer,
    prefix: String,
}

impl<'a> Capture<'a> {
    fn new(gpu: &'a rune::render::Gpu, view: View, stress: bool, large: bool, light: bool) -> Self {
        let content = if stress { "long" } else { "ordinary" };
        let text = if large { "ax5" } else { "standard" };
        let theme = if light { "light" } else { "dark" };
        let format = rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix();
        let mut renderer = rune::render::Renderer::new(&gpu.device, format);
        renderer.set_srgb_blend(true);
        Self {
            gpu,
            renderer,
            prefix: format!("acp-form-{}-{content}-{text}-{theme}", view.name),
        }
    }

    fn frame(&mut self, rt: &mut Runtime<Desk>, scale: u32, stage: &str) -> usize {
        settle(rt);
        let size = (rt.viewport.0 as u32, rt.viewport.1 as u32);
        let pixels = (size.0 * scale, size.1 * scale);
        let rgba =
            crate::ui::offscreen_as_app_with_renderer(self.gpu, &mut self.renderer, rt, pixels);
        assert_eq!(rt.env.scale, scale as f32, "true runtime scale");
        assert_eq!(rgba.len(), (pixels.0 * pixels.1 * 4) as usize);
        let name = format!("{}-{stage}-{}x{}-{scale}x", self.prefix, size.0, size.1);
        if let Some(directory) = std::env::var_os("RUNE_INBOX_SNAP") {
            crate::test_support::visual::write(
                &std::path::PathBuf::from(directory).join(format!("{name}.png")),
                rt,
                pixels,
                &rgba,
                &name,
            );
        }
        1
    }
}

fn phone_frames(
    gpu: &rune::render::Gpu,
    view: View,
    stress: bool,
    large: bool,
    light: bool,
) -> usize {
    let (client, mut rt) = scene(view, stress, large, light);
    let mut capture = Capture::new(gpu, view, stress, large, light);
    let mut count = capture.frame(&mut rt, view.scale, "top");
    select(&mut rt, stress);
    if !stress && view.editing.is_some() {
        count += capture.frame(&mut rt, view.scale, "selected");
    }
    edit(&mut rt, view);
    count += capture.frame(&mut rt, view.scale, "field-focus");
    show(&mut rt, SUBMIT_KEY, true);
    if view.editing.is_some() {
        count += capture.frame(&mut rt, view.scale, "submit-revealed");
    }
    show(&mut rt, CANCEL_KEY, true);
    count += capture.frame(&mut rt, view.scale, "final-action");
    if stress && view.editing.is_some() {
        heading(&mut rt);
        count += capture.frame(&mut rt, view.scale, "field-heading");
    }
    assert_eq!(draft(&rt.state).text("note"), NOTE);
    assert!(rt.state.inbox.questions.pending.is_empty());
    no_input(&client);
    count
}

fn desktop_frames(gpu: &rune::render::Gpu, view: View, stress: bool, light: bool) -> usize {
    let (client, mut rt) = scene(view, stress, false, light);
    let mut capture = Capture::new(gpu, view, stress, false, light);
    let mut count = capture.frame(&mut rt, 1, "top");
    edit(&mut rt, view);
    show(&mut rt, SUBMIT_KEY, false);
    show(&mut rt, CANCEL_KEY, false);
    count += capture.frame(&mut rt, 1, "final-action");
    assert!(rt.state.inbox.questions.pending.is_empty());
    no_input(&client);
    count
}

/// Keep every actual baseline frame while preserving the strict failure: only
/// the error assertion is deferred to the exporter's end. Never clear errors
/// between Submit and Cancel; the latter observation includes inherited state,
/// not an independent Cancel trial (held_actions tests use fresh fixtures).
fn held_frame(gpu: &rune::render::Gpu, view: View, light: bool, errors: &mut Vec<String>) -> usize {
    let (client, mut rt) = scene(view, false, true, light);
    select(&mut rt, false);
    edit(&mut rt, view);
    assert!(
        rt.state.inbox.questions.error.is_empty(),
        "before holding the form: {:?}",
        rt.state.inbox.questions.error
    );
    // Seed an in-flight receipt, like the existing questions unit fixture.
    // This is not an assertion that a native-agent command was delivered.
    rt.update(|d, tx| {
        let owner = owner(d).clone();
        d.inbox.questions.pending.insert(7, owner.clone());
        d.inbox
            .questions
            .status
            .insert(owner, crate::desk::questions::SUBMITTING.into());
        tx.instant();
    });
    settle(&mut rt);
    for part in [SUBMIT_KEY, CANCEL_KEY] {
        let before = rt.state.inbox.questions.error.clone();
        show(&mut rt, part, true);
        let key = key(&rt, part);
        touch(&mut rt, &key, TouchPhase::Ended);
        if !rt.state.inbox.questions.error.is_empty() {
            let theme = if light { "light" } else { "dark" };
            errors.push(format!(
                "{} {theme} AX5, sequential Submit then Cancel, after {key}: \
                 before {before:?}; after {:?}; status {:?}",
                view.name, rt.state.inbox.questions.error, rt.state.inbox.questions.status
            ));
        }
    }
    assert_eq!(rt.state.inbox.questions.pending.len(), 1);
    assert_eq!(&rt.state.inbox.questions.pending[&7], owner(&rt.state));
    assert_eq!(
        rt.state.inbox.questions.status[owner(&rt.state)],
        crate::desk::questions::SUBMITTING
    );
    assert_eq!(draft(&rt.state).text("note"), NOTE);
    assert_eq!(draft(&rt.state).choices["mode"], ["\"safe\""]);
    assert_eq!(draft(&rt.state).choices["tags"], ["\"input\""]);
    rt.ime(rune::Ime::Commit(
        "must not enter a held form or terminal".into(),
    ));
    settle(&mut rt);
    assert_eq!(draft(&rt.state).text("note"), NOTE);
    rt.update(|_, tx| tx.scroll_to_instant("inbox-scroll", 0.0, 100_000.0));
    let mut capture = Capture::new(gpu, view, false, true, light);
    let count = capture.frame(&mut rt, view.scale, "held");
    no_input(&client);
    count
}

fn error_frame(gpu: &rune::render::Gpu, view: View, light: bool) -> usize {
    let (client, mut rt) = scene(view, false, true, light);
    edit(&mut rt, view);
    // The actual Submit handler validates the untouched required mode; no
    // injected error string and no native-agent/terminal fallback traffic.
    activate(&mut rt, SUBMIT_KEY);
    assert_eq!(
        rt.state.inbox.questions.error[owner(&rt.state)],
        "Choose an explicit option for ‘mode’."
    );
    assert_eq!(draft(&rt.state).text("note"), NOTE);
    assert!(draft(&rt.state).choices.is_empty());
    assert!(rt.state.inbox.questions.pending.is_empty());
    rt.update(|_, tx| tx.scroll_to_instant("inbox-scroll", 0.0, 100_000.0));
    let mut capture = Capture::new(gpu, view, false, true, light);
    let count = capture.frame(&mut rt, view.scale, "error");
    no_input(&client);
    count
}

#[test]
fn acp_form_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    let mut count = 0;
    let mut errors = Vec::new();
    for light in [false, true] {
        for stress in [false, true] {
            for large in [false, true] {
                for view in PHONES {
                    count += phone_frames(&gpu, view, stress, large, light);
                }
            }
            for view in DESKTOPS {
                count += desktop_frames(&gpu, view, stress, light);
            }
        }
        for view in [PHONES[2], PHONES[3]] {
            count += held_frame(&gpu, view, light, &mut errors);
            count += error_frame(&gpu, view, light);
        }
    }
    assert_eq!(
        count, 152,
        "keep scripts/visual_acp.py's required inventory matched"
    );
    assert!(
        errors.is_empty(),
        "all 152 frames retained, but held controls changed errors (NOT a visual pass):\n{}",
        errors.join("\n")
    );
}
