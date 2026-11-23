//! Initial action names and complete targets, without manufacturing a scroll.
use super::*;

fn initial_action_names(rt: &Runtime<Desk>) {
    let scroll = crate::ui::element(rt, "inbox-scroll");
    let list = crate::ui::element(rt, "inbox-list");
    assert!(
        (list.rect[1] - scroll.rect[1]).abs() < 0.5,
        "initial discovery must not be manufactured by scrolling"
    );
    assert!(!crate::ui::element(rt, &key(rt, NOTE_KEY)).focused);
    whole(rt, &key(rt, SUBMIT_KEY), true);
    whole(rt, &key(rt, CANCEL_KEY), true);
    let elements = rt.elements();
    for (part, words) in [
        (SUBMIT_KEY, "Submit response"),
        (CANCEL_KEY, "Cancel request"),
    ] {
        let key = key(rt, part);
        let action = crate::ui::element(rt, &key);
        assert!(action.rect[2] >= 44.0 && action.rect[3] >= 44.0);
        assert!(action.focusable);
        let prefix = format!("{}/", action.path);
        let label = elements
            .iter()
            .find(|e| e.path.starts_with(&prefix) && e.text.as_deref() == Some(words))
            .expect("the action's actual rendered name");
        assert!(label.opacity >= 0.999);
        assert!(label.rect[2] > 0.0 && label.rect[3] > 0.0);
        for (full, visible) in label.rect.into_iter().zip(label.visible) {
            assert!(
                (full - visible).abs() < 0.5,
                "{key}: readable whole name {:?}, visible {:?}",
                label.rect,
                label.visible
            );
        }
        let [x, y, w, h] = label.rect;
        let [sx, sy, sw, sh] = scroll.visible;
        assert!(x >= sx - 0.5 && x + w <= sx + sw + 0.5);
        assert!(y >= sy - 0.5 && y + h <= sy + sh + 0.5);
    }
}

/// Unlike `activate`, this must never reveal or scroll the selected option.
fn choose_in_place(rt: &mut Runtime<Desk>, part: &str) {
    let key = key(rt, part);
    whole(rt, &key, true);
    let (x, y) = rt.find(&key).unwrap();
    rt.touch(1, TouchPhase::Started, x, y, 0.1);
    rt.frame(1.0 / 60.0);
    rt.frame(1.0 / 60.0);
    rt.touch(1, TouchPhase::Ended, x, y, 0.15);
    settle(rt);
}

#[test]
fn standard_phone_form_discloses_both_action_names_before_scrolling() {
    for view in [PHONES[0], PHONES[2]] {
        for light in [false, true] {
            let (client, mut rt) = scene(view, false, false, light);
            initial_action_names(&rt);
            choose_in_place(&mut rt, "mode-\"safe\"");
            choose_in_place(&mut rt, "tags-\"input\"");
            initial_action_names(&rt);
            let draft = draft(&rt.state);
            assert_eq!(draft.choices["mode"], ["\"safe\""]);
            assert_eq!(draft.choices["tags"], ["\"input\""]);
            assert!(rt.state.inbox.questions.error.is_empty());
            assert!(rt.state.inbox.questions.pending.is_empty());
            assert!(rt.state.inbox.open);
            no_input(&client);
        }
    }
}
