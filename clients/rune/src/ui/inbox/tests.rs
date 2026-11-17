//! The inbox drawer: its empty state, its answer chips and its rules.

use phux_client_runtime::control::Event;
use rune::Runtime;

use crate::desk::Desk;

fn asked(rt: &mut Runtime<Desk>, suggestions: &[&str]) {
    let terminal_id = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    rt.state.connection.topology = rt
        .state
        .connection
        .client
        .as_ref()
        .and_then(|c| c.topology());
    rt.state.activities.apply(
        &Event::AgentAsked {
            terminal_id,
            question_id: "q-1".into(),
            text: "Continue?".into(),
            suggestions: suggestions.iter().map(|s| (*s).to_owned()).collect(),
            waiting_seconds: None,
        },
        &[],
    );
}

#[test]
fn an_empty_inbox_says_nothing_needs_you_under_the_done_mark() {
    let (_, mut rt) = crate::test_support::input_fixture();
    rt.update(|d, _| d.toggle_inbox());
    rt.frame(0.0);
    let text = rt.accessibility_json().to_string();
    assert!(text.contains("Nothing needs you"));
    for key in [
        "inbox-header-rule",
        "inbox-footer-rule",
        "inbox",
        "inbox-scrim",
    ] {
        assert!(rt.find(key).is_some(), "{key}");
    }
}

#[test]
fn suggested_answers_are_named_buttons() {
    let (_, mut rt) = crate::test_support::input_fixture();
    rt.frame(0.0);
    asked(&mut rt, &["yes", "no"]);
    rt.update(|d, _| d.toggle_inbox());
    rt.frame(0.0);
    let chips: Vec<_> = (rt.elements().into_iter())
        .filter(|e| e.key.as_deref().is_some_and(|k| k.starts_with("chip-")))
        .collect();
    assert_eq!(chips.len(), 2, "one chip per answer");
    for chip in &chips {
        assert_eq!(chip.role, "button");
        assert!(chip.focusable, "a chip is a Tab stop");
        assert!(matches!(chip.label.as_deref(), Some("yes" | "no")));
    }
}
