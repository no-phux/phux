use rune::{Env, Runtime, TextEdit};

use crate::desk::Desk;

fn shows(rt: &Runtime<Desk>, words: &str) -> bool {
    rt.elements()
        .iter()
        .any(|e| e.text.as_deref().is_some_and(|t| t.contains(words)))
}

#[test]
fn the_composer_names_its_session_and_says_why_nothing_went() {
    let mut desk = crate::test_support::window();
    desk.file.chief = Some("staff".into());
    desk.open_chief();
    desk.chief.draft = TextEdit::new("move the release to Friday");
    desk.send_chief();
    let mut rt =
        crate::test_support::runtime(desk, crate::ui::view, (900.0, 700.0), Env::new(true, 1.0));
    rt.frame(0.0);
    for key in ["chief", "chief-draft", "chief-error", "chief-footer-rule"] {
        assert!(rt.find(key).is_some(), "{key} missing");
    }
    assert!(
        rt.find("chief-status").is_none(),
        "nothing went, so no receipt"
    );
    assert!(shows(&rt, "session ‘staff’"));
    assert!(shows(&rt, "Not connected. Nothing was sent."));

    rt.update(|d: &mut Desk, _| d.close_chief());
    rt.frame(0.5);
    assert!(rt.find("chief-draft").is_none());
}
