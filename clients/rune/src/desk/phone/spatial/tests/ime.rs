//! An inline ACP field, unlike an inbox sheet, does not open an app modal.
use super::*;
use phux_protocol::native_agent::{NativeCapabilities, NativeQuestion, NativeView};

const QUESTION: &str = "acp-question:inline";

#[test]
fn inline_question_fields_take_the_terminal_ime_and_return_it_on_blur() {
    for phone in [false, true] {
        for option_as_meta in [false, true] {
            for kind in ["input", "editor"] {
                let (client, mut rt) = fixture(phone);
                rt.set_viewport(
                    if phone {
                        (393.0, 852.0)
                    } else {
                        (1100.0, 720.0)
                    },
                    2.0,
                );
                let question = NativeQuestion {
                    id: QUESTION.into(),
                    kind: kind.into(),
                    title: "Reply".into(),
                    message: "Type here".into(),
                    options: vec![],
                    questions: vec![],
                    fields: serde_json::Value::Null,
                };
                rt.update(|d, tx| {
                    tx.instant();
                    d.settings.option_as_meta = option_as_meta;
                    d.inbox.questions.drafts.insert(
                        question.id.clone(),
                        crate::desk::questions::Draft::new(&question),
                    );
                    d.inbox.questions.views.insert(
                        id(1),
                        (
                            id(7),
                            NativeView {
                                activation: "inline".into(),
                                backend: "acp".into(),
                                capabilities: NativeCapabilities {
                                    questions: true.into(),
                                    ..NativeCapabilities::default()
                                },
                                connected: true,
                                turn_id: None,
                                provider_session_id: None,
                                questions: vec![question],
                            },
                        ),
                    );
                });
                settle(&mut rt);
                assert!(rt.state.modal().is_none());
                assert!(rt.find("ime-caret").is_some(), "terminal owns the IME");
                let key = format!("acp-{QUESTION}-field-text");
                let (x, y) = rt.find(&key).expect("inline field");
                rt.pointer_down(x, y);
                rt.pointer_up(x, y);
                rt.frame(0.0);
                assert!(rt.state.modal().is_none(), "this is not an inbox sheet");
                assert!(rt.state.inbox.questions.field_focused(&id(1), QUESTION));
                assert!(rt.find("ime-caret").is_none(), "no terminal Option-as-Alt");
                let _ = outbound(&client);
                rt.ime(rune::Ime::Commit("∫".into()));
                rt.frame(0.0);
                assert_eq!(rt.state.inbox.questions.drafts[QUESTION].text("text"), "∫");
                no_input(&client);

                // Click below the card, not its controls or the phone's toolbar.
                let [x, y] = rt.state.origins[&id(1)];
                let height = rt.state.pane_area(&id(1)).unwrap()[1];
                rt.pointer_down(x + 10.0, y + height - 25.0);
                rt.pointer_up(x + 10.0, y + height - 25.0);
                rt.frame(0.0);
                assert!(!rt.state.inbox.questions.field_focused(&id(1), QUESTION));
                assert!(rt.find("ime-caret").is_some(), "terminal IME restored");
            }
        }
    }
}
