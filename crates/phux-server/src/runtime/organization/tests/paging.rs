//! Paging: extracted from the shared module.
use super::*;

#[test]
pub(super) fn deleting_before_the_page_cursor_does_not_skip_the_next_record() {
    run(async {
        let mut org = Org::new();
        for name in ["alpha", "bravo", "charlie", "delta"] {
            org.create_session(name);
        }
        let all = org.list(OrgFilter::All, 0, None);
        let first = org.list(OrgFilter::All, 2, None);
        assert_eq!(first.sessions.len(), 2);
        let cursor_key = first.sessions[1].key;
        let removed = first
            .sessions
            .iter()
            .rev()
            .find(|session| session.name != phux_core::org::INBOX_NAME)
            .expect("a deletable session on the first page")
            .clone();
        let cursor = first.next_cursor.expect("a later page");
        assert_eq!(cursor.as_slice(), cursor_key.as_bytes());
        org.delete_session(&removed);
        let (later, _) = org.drain(OrgFilter::All, 2, Some(cursor));
        let seen: Vec<_> = later.iter().map(|session| session.key).collect();
        let expected: Vec<_> = all
            .sessions
            .iter()
            .map(|session| session.key)
            .filter(|key| *key > cursor_key)
            .collect();
        assert_eq!(seen, expected, "a session was skipped or repeated");
        assert!(!seen.contains(&removed.key), "deleted session returned");

        let mut org = Org::new();
        let session = org.create_session("work");
        for _ in 0..4 {
            org.create_resource(session);
        }
        let full = org.list(OrgFilter::Session(session), 0, None);
        let first = org.list(OrgFilter::Session(session), 2, None);
        assert_eq!(
            first.sessions.len(),
            1,
            "the session record leads its members"
        );
        assert_eq!(first.resources.len(), 1);
        let emitted = first.resources[0].clone();
        let cursor = first.next_cursor.expect("members remain");
        assert_eq!(&cursor[..16], session.as_bytes());
        assert_eq!(&cursor[16..], emitted.key.as_bytes());
        org.remove_resource(&emitted);
        let (later_sessions, later_resources) =
            org.drain(OrgFilter::Session(session), 2, Some(cursor));
        assert!(later_sessions.is_empty(), "the session record was repeated");
        let seen: Vec<_> = later_resources
            .iter()
            .map(|resource| resource.key)
            .collect();
        let expected: Vec<_> = full
            .resources
            .iter()
            .map(|resource| resource.key)
            .filter(|key| *key > emitted.key)
            .collect();
        assert_eq!(seen, expected, "a resource was skipped or repeated");
    });
}

#[test]
pub(super) fn inserting_before_the_page_cursor_does_not_repeat_a_record() {
    run(async {
        let mut org = Org::new();
        for name in ["alpha", "bravo", "charlie", "delta"] {
            org.create_session(name);
        }
        let all = org.list(OrgFilter::All, 0, None);
        let first = org.list(OrgFilter::All, 2, None);
        let cursor_key = first.sessions[1].key;
        let cursor = first.next_cursor.expect("a later page");
        let inserted = insert_session_before(&org.state, cursor_key);
        assert!(inserted < cursor_key);
        let with_insert = org.list(OrgFilter::All, 0, None);
        assert!(
            with_insert
                .sessions
                .iter()
                .any(|session| session.key == inserted),
            "the inserted session is in the organization"
        );
        let (later, _) = org.drain(OrgFilter::All, 2, Some(cursor));
        let seen: Vec<_> = later.iter().map(|session| session.key).collect();
        let expected: Vec<_> = all
            .sessions
            .iter()
            .map(|session| session.key)
            .filter(|key| *key > cursor_key)
            .collect();
        assert_eq!(seen, expected, "a session was skipped or repeated");
        assert!(
            !seen.contains(&inserted),
            "a session inserted into the consumed prefix was emitted"
        );

        let mut org = Org::new();
        let session = org.create_session("work");
        for _ in 0..4 {
            org.create_resource(session);
        }
        let full = org.list(OrgFilter::Session(session), 0, None);
        let first = org.list(OrgFilter::Session(session), 2, None);
        let emitted = first.resources[0].key;
        let cursor = first.next_cursor.expect("members remain");
        let inserted = insert_resource_before(&org.state, session, emitted);
        assert!(inserted < emitted);
        let with_insert = org.list(OrgFilter::Session(session), 0, None);
        assert!(
            with_insert
                .resources
                .iter()
                .any(|resource| resource.key == inserted),
            "the inserted resource is in the organization"
        );
        let (later_sessions, later_resources) =
            org.drain(OrgFilter::Session(session), 2, Some(cursor));
        assert!(later_sessions.is_empty(), "session record repeated");
        let seen: Vec<_> = later_resources
            .iter()
            .map(|resource| resource.key)
            .collect();
        let expected: Vec<_> = full
            .resources
            .iter()
            .map(|resource| resource.key)
            .filter(|key| *key > emitted)
            .collect();
        assert_eq!(seen, expected, "a resource was skipped or repeated");
        assert!(
            !seen.contains(&inserted),
            "a resource inserted into the consumed prefix was emitted"
        );
    });
}
