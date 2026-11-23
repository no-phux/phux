//! Captured native targets cannot edit a replacement question.
use super::*;

#[test]
fn captured_native_fields_refuse_changed_and_reopened_questions() {
    use phux_protocol::ids::ResourceId;
    for reopen in [false, true] {
        let (client, mut rt) = crate::test_support::input_fixture();
        native(&mut rt.state);
        let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
        let question = acp_question("acp-question:1", "Title", "Message");
        rt.state.inbox.questions.views.insert(
            terminal.clone(),
            (ResourceId::local(2), acp_view("same", &question)),
        );
        rt.state.inbox.questions.reconcile();
        let old = rt
            .state
            .inbox
            .questions
            .owner(&terminal, &question.id)
            .unwrap()
            .clone();
        let (field, _) = target::Field::question(&rt.state, &old.field_key("text")).unwrap();
        if reopen {
            rt.state
                .inbox
                .questions
                .views
                .get_mut(&terminal)
                .unwrap()
                .1
                .questions
                .clear();
            rt.state.inbox.questions.reconcile();
            rt.state
                .inbox
                .questions
                .views
                .get_mut(&terminal)
                .unwrap()
                .1
                .questions = vec![question.clone()];
        } else {
            rt.state
                .inbox
                .questions
                .views
                .get_mut(&terminal)
                .unwrap()
                .1
                .questions[0]
                .fields = serde_json::json!({"new": "content"});
        }
        rt.state.inbox.questions.reconcile();
        let current = rt
            .state
            .inbox
            .questions
            .owner(&terminal, &question.id)
            .unwrap()
            .clone();
        assert_ne!(old, current);
        assert!(field.edit(&rt.state).is_none());
        assert!(field.edit_mut(&mut rt.state).is_none());
        let _ = client.take_outbound();
        rt.update(|d, tx| field.key(d, &event("Enter", false, false, false, false).unwrap(), tx));
        assert_eq!(
            rt.state
                .inbox
                .questions
                .draft(&current)
                .unwrap()
                .text("text"),
            ""
        );
        assert!(rt.state.inbox.questions.pending.is_empty());
        assert!(client.take_outbound().is_empty());
    }
}
