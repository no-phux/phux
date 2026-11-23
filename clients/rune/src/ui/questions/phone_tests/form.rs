//! Selected form fixtures shared by baseline and candidate, never gap goldens.
use super::*;

mod discovery;
mod grouping;
mod held_actions;
mod render;

const NOTE: &str = "Keep pairing intact; verify input and pane layout.";
const NOTE_KEY: &str = "acp-acp-question:phone-field-note";
const SUBMIT_KEY: &str = "acp-acp-question:phone-submit";
const CANCEL_KEY: &str = "acp-acp-question:phone-cancel";

#[derive(Clone, Copy)]
struct View {
    name: &'static str,
    size: (u32, u32),
    editing: Option<(u32, u32)>,
    scale: u32,
    phone: bool,
}

// Full logical phone surfaces at 1×, then UIKit-safe-area-shaped surfaces at
// true 2×. Only the latter shrink after field focus, as UIKit's keyboard does.
// No drawn keyboard is used or claimed to prove UIKit behavior.
const PHONES: [View; 4] = [
    View {
        name: "phone",
        size: (393, 852),
        editing: None,
        scale: 1,
        phone: true,
    },
    View {
        name: "small",
        size: (320, 568),
        editing: None,
        scale: 1,
        phone: true,
    },
    View {
        name: "native",
        size: (393, 759),
        editing: Some((393, 480)),
        scale: 2,
        phone: true,
    },
    View {
        name: "native-small",
        size: (320, 478),
        editing: Some((320, 340)),
        scale: 2,
        phone: true,
    },
];

const DESKTOPS: [View; 2] = [
    View {
        name: "desktop",
        size: (1100, 720),
        editing: None,
        scale: 1,
        phone: false,
    },
    View {
        name: "desktop-min",
        size: (640, 360),
        editing: None,
        scale: 1,
        phone: false,
    },
];

fn ordinary() -> NativeQuestion {
    NativeQuestion {
        title: "Review deployment checks".into(),
        message: "Choose a run mode and leave a review note before the agent continues.".into(),
        fields: json!({
            "properties": {
                "mode": {"title": "Run mode", "enum": ["safe", "fast"]},
                "note": {
                    "title": "Review note",
                    "description": "Tell the agent what to verify before it continues.",
                    "type": "string"
                },
                "tags": {
                    "title": "Checks to include",
                    "description": "Select any relevant follow-up checks.",
                    "type": "array", "items": {"enum": ["input", "layout"]}
                }
            },
            "required": ["mode", "note"]
        }),
        ..question("form")
    }
}

fn scene(view: View, stress: bool, large: bool, light: bool) -> (Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture((view.size.0 as f32, view.size.1 as f32), "form");
    rt.set_viewport((view.size.0 as f32, view.size.1 as f32), view.scale as f32);
    rt.set_srgb_blend(true);
    rt.update(|d, tx| {
        d.phone = view.phone.then(crate::desk::phone::Phone::default);
        d.native = view.phone.then(crate::mobile::Platform::test);
        d.settings.theme = crate::theme::find(if light { "phux-light" } else { "phux" });
        d.set_accessibility(
            false,
            if large {
                "UICTContentSizeCategoryAccessibilityXXXL"
            } else {
                "UICTContentSizeCategoryL"
            },
        );
        let asked = if stress { question("form") } else { ordinary() };
        d.inbox
            .questions
            .drafts
            .insert(ID.into(), Draft::new(&asked));
        let terminal = d.connection.shown.as_ref().unwrap().resource.clone();
        d.inbox
            .questions
            .views
            .get_mut(&terminal)
            .unwrap()
            .1
            .questions = vec![asked];
        tx.instant();
    });
    settle(&mut rt);
    assert_eq!(rt.state.inbox_items().len(), 1);
    assert!(rt.state.inbox.questions.drafts[ID].choices.is_empty());
    (client, rt)
}

/// Require complete visible bounds, not merely a 44-point fragment.
fn whole(rt: &Runtime<Desk>, key: &str, phone: bool) {
    let e = crate::ui::element(rt, key);
    for (full, visible) in e.rect.into_iter().zip(e.visible) {
        assert!(
            (full - visible).abs() < 0.5,
            "{key}: {:?} visible {:?}",
            e.rect,
            e.visible
        );
    }
    let [x, y, w, h] = e.rect;
    assert!(
        x >= -0.5 && x + w <= rt.viewport.0 + 0.5,
        "{key}: horizontal bounds"
    );
    assert!(
        y >= -0.5 && y + h <= rt.viewport.1 + 0.5,
        "{key}: vertical bounds"
    );
    if phone {
        assert!(w >= 44.0 && h >= 44.0, "{key}: full touch target");
    }
}

fn show(rt: &mut Runtime<Desk>, key: &str, phone: bool) {
    rt.update(|_, tx| {
        tx.scroll_into_view(key);
        tx.instant();
    });
    settle(rt);
    whole(rt, key, phone);
}

fn activate(rt: &mut Runtime<Desk>, key: &str) {
    show(rt, key, true);
    touch(rt, key, TouchPhase::Ended);
}

fn select(rt: &mut Runtime<Desk>, stress: bool) {
    activate(rt, &format!("acp-{ID}-mode-\"safe\""));
    let tag = if stress { "a" } else { "input" };
    activate(rt, &format!("acp-{ID}-tags-\"{tag}\""));
    let draft = &rt.state.inbox.questions.drafts[ID];
    assert_eq!(draft.choices["mode"], ["\"safe\""]);
    assert_eq!(draft.choices["tags"], [format!("\"{tag}\"")]);
}

fn edit(rt: &mut Runtime<Desk>, view: View) {
    let keyboard = rt.state.phone.as_ref().map(|phone| phone.keyboard);
    show(rt, NOTE_KEY, view.phone);
    assert!(rt.accessibility_action(NOTE_KEY, rune::accesskit::Action::Focus, None));
    settle(rt);
    if let Some(size) = view.editing {
        rt.set_viewport((size.0 as f32, size.1 as f32), view.scale as f32);
    }
    rt.ime(rune::Ime::Commit(NOTE.into()));
    settle(rt);
    show(rt, NOTE_KEY, view.phone);
    assert!(crate::ui::element(rt, NOTE_KEY).focused);
    assert_eq!(rt.state.inbox.questions.drafts[ID].text("note"), NOTE);
    if view.phone {
        let snapshot = crate::desk::native_input::Bridge::default().snapshot(rt);
        assert_eq!(snapshot.kind, "field");
        assert!(snapshot.wanted, "ACP field owns the native keyboard");
        assert_eq!(snapshot.text, NOTE);
        assert_eq!(
            rt.state.phone.as_ref().map(|phone| phone.keyboard),
            keyboard,
            "terminal preference unchanged"
        );
    }
}

/// Bring the note heading to the reading edge independently of editor focus.
/// LONG/AX5 may exceed the entire reduced viewport: record that limit, never
/// assert that showing the editor also showed the complete heading.
fn heading(rt: &mut Runtime<Desk>) {
    rt.update(|_, tx| tx.scroll_to_instant("inbox-scroll", 0.0, 0.0));
    settle(rt);
    let fast = crate::ui::element(rt, &format!("acp-{ID}-mode-\"fast\"")).rect;
    let field = crate::ui::element(rt, NOTE_KEY).rect;
    let title = rt
        .elements()
        .into_iter()
        .find(|e| {
            e.path.contains("/inbox/")
                && e.text.as_deref() == Some(LONG)
                && e.rect[1] >= fast[1] + fast[3]
                && e.rect[1] < field[1]
        })
        .expect("the note heading, not the identical message or description");
    let scroll = crate::ui::element(rt, "inbox-scroll").rect;
    rt.update(|_, tx| tx.scroll_to_instant("inbox-scroll", 0.0, title.rect[1] - scroll[1]));
    settle(rt);
    let shown = rt
        .elements()
        .into_iter()
        .find(|e| e.path == title.path)
        .unwrap();
    eprintln!(
        "ACP form heading: viewport={:?} rect={:?} visible={:?}; full={}",
        rt.viewport,
        shown.rect,
        shown.visible,
        shown.rect == shown.visible
    );
    assert!(
        shown.visible[2] >= shown.rect[2] - 0.5,
        "the heading keeps its complete reading width"
    );
    let available = crate::ui::element(rt, "inbox-scroll").visible[3];
    if shown.rect[3] <= available {
        assert!(
            shown.visible[3] >= shown.rect[3] - 0.5,
            "a heading that fits must be fully revealed"
        );
    }
    // This diagnostic intentionally reports natural overflow; no content,
    // scale, or production assertion is shortened to obtain a green image.
}

#[test]
fn acp_form_fixture_choices_draft_and_final_controls_keep_ownership() {
    for view in [PHONES[0], PHONES[3]] {
        for keyboard in [false, true] {
            let (client, mut rt) = scene(view, false, true, false);
            rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = keyboard);
            settle(&mut rt);
            activate(&mut rt, "acp-acp-question:phone-mode-\"fast\"");
            activate(&mut rt, "acp-acp-question:phone-mode-\"safe\"");
            activate(&mut rt, "acp-acp-question:phone-tags-\"input\"");
            activate(&mut rt, "acp-acp-question:phone-tags-\"layout\"");
            activate(&mut rt, "acp-acp-question:phone-tags-\"layout\"");
            assert_eq!(
                rt.state.inbox.questions.drafts[ID].choices["mode"],
                ["\"safe\""]
            );
            assert_eq!(
                rt.state.inbox.questions.drafts[ID].choices["tags"],
                ["\"input\""]
            );
            edit(&mut rt, view);
            for key in [SUBMIT_KEY, CANCEL_KEY] {
                show(&mut rt, key, true);
                touch(&mut rt, key, TouchPhase::Cancelled);
            }
            assert!(rt.state.inbox.open);
            assert_eq!(rt.state.inbox.questions.drafts[ID].text("note"), NOTE);
            assert!(rt.state.inbox.questions.pending.is_empty());
            assert!(rt.state.inbox.questions.error.is_empty());
            assert_eq!(
                rt.state.phone.as_ref().unwrap().keyboard,
                keyboard,
                "choices and final controls preserve the terminal preference"
            );
            no_input(&client);
        }
    }
}
