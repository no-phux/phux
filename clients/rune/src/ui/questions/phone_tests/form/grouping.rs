//! Association is layout, not a new card or a different form response.
use super::*;

fn gap(above: [f32; 4], below: [f32; 4], expected: f32) {
    let actual = below[1] - above[1] - above[3];
    assert!(
        (actual - expected).abs() < 0.5,
        "expected {expected} between {above:?} and {below:?}, got {actual}"
    );
}

#[test]
fn phone_form_groups_related_parts_more_closely_than_fields_and_final_actions() {
    let s = phux_design::tokens::SPACE;
    for view in PHONES {
        for stress in [false, true] {
            for large in [false, true] {
                let (client, rt) = scene(view, stress, large, false);
                let groups = ["mode", "note", "tags"]
                    .map(|id| crate::ui::element(&rt, &owner(&rt.state).form_key(id)));
                gap(groups[0].rect, groups[1].rect, s.s3);
                gap(groups[1].rect, groups[2].rect, s.s3);
                gap(
                    groups[2].rect,
                    crate::ui::element(&rt, &key(&rt, SUBMIT_KEY)).rect,
                    s.s4,
                );
                gap(
                    crate::ui::element(&rt, &key(&rt, SUBMIT_KEY)).rect,
                    crate::ui::element(&rt, &key(&rt, CANCEL_KEY)).rect,
                    s.icon_gap,
                );
                let elements = rt.elements();
                let note_path = format!("{}/", groups[1].path);
                let heading = elements
                    .iter()
                    .find(|e| e.path.starts_with(&note_path) && e.text.is_some())
                    .expect("the note's own heading");
                let parts: Vec<_> = elements
                    .iter()
                    .filter(|e| e.parent == heading.parent)
                    .collect();
                assert_eq!(parts.len(), 3, "heading, description and field shell");
                for pair in parts.windows(2) {
                    gap(pair[0].rect, pair[1].rect, s.s1);
                }
                assert!(s.s3 > s.s1, "between-field space is visibly distinct");
                assert!(!groups[1].focusable, "grouping adds no phantom control");
                assert!(rt.state.inbox.questions.error.is_empty());
                no_input(&client);
            }
        }
    }
}

#[test]
fn desktop_form_keeps_its_flat_layout_and_original_spacing() {
    let s = phux_design::tokens::SPACE;
    for view in DESKTOPS {
        let (client, rt) = scene(view, false, false, false);
        let elements = rt.elements();
        assert!(!elements.iter().any(|e| {
            e.key
                .as_deref()
                .is_some_and(|key| key.starts_with("acp-form-"))
        }));
        let heading = elements
            .iter()
            .find(|e| e.text.as_deref() == Some("Review note *"))
            .unwrap();
        let description = elements
            .iter()
            .find(|e| {
                e.text.as_deref() == Some("Tell the agent what to verify before it continues.")
            })
            .unwrap();
        assert_eq!(heading.parent, description.parent, "flat panel children");
        gap(
            crate::ui::element(&rt, &key(&rt, Widget::Choices("mode"))).rect,
            heading.rect,
            s.s2,
        );
        gap(heading.rect, description.rect, s.s2);
        gap(
            crate::ui::element(&rt, &key(&rt, Widget::Choice("tags", "\"layout\""))).rect,
            crate::ui::element(&rt, &key(&rt, SUBMIT_KEY)).rect,
            s.s2,
        );
        no_input(&client);
    }
}

#[test]
fn enabled_phone_form_still_reports_a_genuine_validation_error() {
    for view in [PHONES[0], PHONES[1]] {
        let (client, mut rt) = scene(view, false, false, false);
        edit(&mut rt, view);
        activate(&mut rt, SUBMIT_KEY);
        assert_eq!(
            rt.state.inbox.questions.error[owner(&rt.state)],
            "Choose an explicit option for ‘mode’."
        );
        assert_eq!(draft(&rt.state).text("note"), NOTE);
        assert!(draft(&rt.state).choices.is_empty());
        assert!(rt.state.inbox.questions.pending.is_empty());
        assert!(rt.state.inbox.open);
        no_input(&client);
    }
}

#[test]
fn phone_form_close_preserves_acp_draft_and_both_terminal_keyboard_preferences() {
    for view in [PHONES[0], PHONES[1]] {
        for keyboard in [false, true] {
            let (client, mut rt) = scene(view, false, false, false);
            rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = keyboard);
            settle(&mut rt);
            select(&mut rt, false);
            edit(&mut rt, view);
            reveal(&mut rt, "inbox-close");
            whole(&rt, "inbox-close", true);
            touch(&mut rt, "inbox-close", TouchPhase::Ended);
            assert!(!rt.state.inbox.open);
            assert_eq!(draft(&rt.state).text("note"), NOTE);
            assert_eq!(draft(&rt.state).choices["mode"], ["\"safe\""]);
            assert_eq!(draft(&rt.state).choices["tags"], ["\"input\""]);
            assert_eq!(rt.state.phone.as_ref().unwrap().keyboard, keyboard);
            assert!(rt.state.inbox.questions.error.is_empty());
            assert!(rt.state.inbox.questions.pending.is_empty());
            no_input(&client);
        }
    }
}
