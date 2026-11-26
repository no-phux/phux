//! No-match feedback must not turn an editable query into a second long document.
use super::*;
use crate::ui::element;

#[test]
fn no_match_feedback_stays_concise_without_changing_the_search_or_terminal() {
    for size in [(320.0, 340.0), (393.0, 480.0)] {
        for category in [
            "UICTContentSizeCategoryL",
            "UICTContentSizeCategoryAccessibilityXXXL",
        ] {
            let (client, mut rt) = fixture();
            detailed_inventory(&mut rt);
            rt.set_viewport(size, 2.0);
            rt.update(|d, _| {
                d.native = Some(crate::mobile::Platform::test());
                d.set_accessibility(false, category);
                d.phone.as_mut().unwrap().keyboard = false;
                d.open_phone_sessions();
            });
            settle(&mut rt);
            tap(&mut rt, "phone-session-query", 0.1);
            let shown = rt.state.connection.shown.as_ref().unwrap().resource.clone();
            let session = client.selected_session();
            let mut heights = Vec::new();
            for query in [
                "nothing matches".to_owned(),
                "workspace_build_integration_review_authentication_long_query".repeat(8),
            ] {
                rt.key(rune::KeyEvent {
                    key: Key::Command('a'),
                    mods: rune::Mods {
                        cmd: true,
                        ..rune::Mods::default()
                    },
                });
                rt.key(crate::keys::plain(Key::Text(query.clone())));
                rt.settle_all();
                settle(&mut rt);
                let feedback = element(&rt, "phone-session-empty");
                let prefix = format!("{}/", feedback.path);
                let words: Vec<_> = rt
                    .elements()
                    .into_iter()
                    .filter(|e| e.path.starts_with(&prefix))
                    .filter_map(|e| e.text)
                    .collect();
                assert_eq!(
                    words,
                    ["No matches", "Try another name or clear your search."]
                );
                heights.push(feedback.rect[3]);
                let phone = rt.state.phone.as_ref().unwrap();
                assert_eq!(phone.query.text, query);
                assert!(phone.query.focused);
                assert!(!phone.keyboard, "terminal keyboard preference is unchanged");
                assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, shown);
                assert_eq!(client.selected_session(), session);
            }
            assert!((heights[0] - heights[1]).abs() < 0.5, "{heights:?}");
            tap(&mut rt, "phone-session-clear", 0.3);
            assert!(rt.find("phone-session-empty").is_none());
            assert!(rt.find("phone-session-1").is_some());
            no_terminal_input(&client);
        }
    }
}
