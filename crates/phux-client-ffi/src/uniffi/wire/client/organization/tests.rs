use super::*;
use phux_protocol::ids::Revision;

#[test]
fn create_launches_only_with_a_directory_or_for_an_ephemeral_session() {
    assert!(create_launch(WireSessionLifetime::Persistent, None).is_none());
    assert!(create_launch(WireSessionLifetime::Ephemeral, None).is_some());
    assert!(create_launch(WireSessionLifetime::Persistent, Some("/tmp".into())).is_some());
}

/// Create and rename judge a name by core's one rule; a refusal is
/// `invalid_session_name` before any connection is needed.
#[test]
fn create_and_rename_refuse_what_core_refuses() {
    fn word<T>(result: Result<T, WireError>) -> Option<String> {
        match result {
            Err(WireError::Runtime { reason }) => Some(reason),
            _ => None,
        }
    }
    let too_long = "a".repeat(phux_client_core::rename::MAX_SESSION_NAME_BYTES + 1);
    let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
    let key = SessionKey::new([2; 16]).unwrap().to_string();
    for bad in ["x:y", too_long.as_str(), "a\tb", "a\0b", "@3"] {
        assert_eq!(
            word(remote.create_session(bad.to_owned(), WireSessionLifetime::Persistent, None))
                .as_deref(),
            Some("invalid_session_name"),
            "create {bad:?}"
        );
        assert_eq!(
            word(remote.rename_session(key.clone(), bad.to_owned())).as_deref(),
            Some("invalid_session_name"),
            "rename {bad:?}"
        );
    }
    let at_limit = "a".repeat(phux_client_core::rename::MAX_SESSION_NAME_BYTES);
    for good in ["notes", at_limit.as_str()] {
        assert_eq!(
            word(remote.create_session(good.to_owned(), WireSessionLifetime::Persistent, None))
                .as_deref(),
            Some("not_connected"),
            "create {good:?}"
        );
        assert_eq!(
            word(remote.rename_session(key.clone(), good.to_owned())).as_deref(),
            Some("not_connected"),
            "rename {good:?}"
        );
    }
}

#[test]
fn created_session_key_is_correlated_with_its_committed_operation() {
    // No topology or session name is needed, including for an empty session.
    for byte in [0x12, 0x34] {
        let operation = OperationId::new([byte; 16]).unwrap();
        let session = SessionKey::new([byte + 1; 16]).unwrap();
        let committed = Committed::new(operation, Revision::new(9)).with_session(Some(session));
        assert_eq!(
            lower(&Event::OrganizationOperation {
                operation,
                outcome: OrgOutcome::Committed(Box::new(committed)),
            }),
            Some(WireEvent::OrganizationOperation {
                operation: operation_text(&operation),
                status: "committed".to_owned(),
                refusal: 0,
                refusal_word: String::new(),
                conflict_revision: None,
                revision: Some(9),
                message: String::new(),
                undoable: true,
                created_session_key: Some(session.to_string()),
                ensured_created: None,
            })
        );
    }
}

#[test]
fn receipts_are_bounded_oldest_first() {
    let mut receipts = VecDeque::new();
    for n in 0..=MAX_RECEIPTS {
        let byte = u8::try_from(n % 255 + 1).unwrap();
        let mut bytes = [byte; 16];
        bytes[0] = u8::try_from(n / 255).unwrap();
        let committed = Committed::new(OperationId::new(bytes).unwrap(), Revision::new(1));
        remember(&mut receipts, &committed);
    }
    assert_eq!(receipts.len(), MAX_RECEIPTS);
}
