use super::*;

#[test]
fn arrangements_read_and_replace_by_revision() {
    run(async {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let (session, pane) = state.with_mut(|s| s.seed_session("work"));
        let (key, resource) = state.with(|s| {
            let record = s.registry().session(session).unwrap();
            (
                ow::arrangement_key(record.arrangement.unwrap()).unwrap(),
                ow::resource_key(s.registry().resource_key(pane).unwrap()).unwrap(),
            )
        });
        let get = || {
            handle(
                &state,
                CLIENT,
                Command::ArrangementGet { arrangement: key },
                &root,
            )
        };
        let CommandResult::OkWith(CommandValue::Arrangement(value)) = get() else {
            panic!("arrangement");
        };
        assert_eq!(value.revision, Revision::new(1));
        assert_eq!(
            value.document.groups.len(),
            1,
            "the seeded terminal is arranged at creation"
        );

        let document = ArrangementDoc {
            groups: vec![phux_protocol::wire::frame::ArrangementGroup {
                locked: false,
                id: [1; 16],
                name: None,
                tree: phux_protocol::wire::frame::ArrangementNode::Leaf(
                    phux_protocol::wire::frame::EntryRef {
                        entry: [2; 16],
                        target: phux_protocol::wire::frame::EntryTarget::Member(resource),
                    },
                ),
            }],
        };
        let replace = |n, expected| Command::ArrangementReplace {
            operation: op(n),
            arrangement: key,
            expected: Revision::new(expected),
            document: document.clone(),
        };
        assert_eq!(
            committed(handle(&state, CLIENT, replace(1, 1), &root))
                .revision
                .get(),
            2
        );
        assert_eq!(
            handle(&state, CLIENT, replace(2, 1), &root),
            CommandResult::Refused(WireRefusal::Conflict {
                current: Revision::new(2)
            })
        );
        let CommandResult::OkWith(CommandValue::Arrangement(value)) = get() else {
            panic!("arrangement");
        };
        assert_eq!(value.document, document);
    });
}

/// A stale `ARRANGEMENT_REPLACE` is refused and the refusal is visible at
/// info, which is what a default `phux=info` log shows.
#[test]
fn a_stale_arrangement_replace_is_logged() {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let state = SharedState::new();
    let root = CancellationToken::new();
    let (session, pane) = state.with_mut(|s| s.seed_session("work"));
    let (key, resource) = state.with(|s| {
        let record = s.registry().session(session).unwrap();
        (
            ow::arrangement_key(record.arrangement.unwrap()).unwrap(),
            ow::resource_key(s.registry().resource_key(pane).unwrap()).unwrap(),
        )
    });
    let document = ArrangementDoc {
        groups: vec![phux_protocol::wire::frame::ArrangementGroup {
            locked: false,
            id: [1; 16],
            name: None,
            tree: phux_protocol::wire::frame::ArrangementNode::Leaf(
                phux_protocol::wire::frame::EntryRef {
                    entry: [2; 16],
                    target: phux_protocol::wire::frame::EntryTarget::Member(resource),
                },
            ),
        }],
    };
    let replace = |n, expected| Command::ArrangementReplace {
        operation: op(n),
        arrangement: key,
        expected: Revision::new(expected),
        document: document.clone(),
    };
    assert_eq!(
        committed(handle(&state, CLIENT, replace(1, 1), &root))
            .revision
            .get(),
        2
    );

    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(buf.clone())
        .finish();
    let refused = tracing::subscriber::with_default(subscriber, || {
        tracing::callsite::rebuild_interest_cache();
        handle(&state, CLIENT, replace(2, 1), &root)
    });
    assert_eq!(
        refused,
        CommandResult::Refused(WireRefusal::Conflict {
            current: Revision::new(2)
        })
    );
    let log = String::from_utf8(buf.0.lock().expect("log").clone()).unwrap();
    assert!(
        log.contains("arrangement save refused") && log.contains("conflict"),
        "{log}"
    );
}

/// `ARRANGEMENT_LOCK` sets the flag the arrangement then reports, and a
/// legacy kill of the locked tab's last pane is refused, killing nothing,
/// until it is unlocked.
#[test]
fn a_locked_tab_refuses_a_kill_until_unlocked() {
    run(async {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let (session, pane) = state.with_mut(|s| s.seed_session("work"));
        let (key, wire_pane) = state.with_mut(|s| {
            let record = s.registry().session(session).unwrap();
            let key = ow::arrangement_key(record.arrangement.unwrap()).unwrap();
            (key, s.intern_resource_wire(pane))
        });
        let get = || match handle(
            &state,
            CLIENT,
            Command::ArrangementGet { arrangement: key },
            &root,
        ) {
            CommandResult::OkWith(CommandValue::Arrangement(value)) => value.document,
            other => panic!("arrangement, got {other:?}"),
        };
        let group = get().groups[0].id;
        let lock = |n, locked| Command::ArrangementLock {
            operation: op(n),
            arrangement: key,
            group,
            locked,
        };
        committed(handle(&state, CLIENT, lock(1, true), &root));
        assert!(get().groups[0].locked);

        let kill = || {
            crate::runtime::commands::handle_kill_terminals(
                &state,
                CLIENT,
                std::slice::from_ref(&wire_pane),
                None,
            )
        };
        assert!(
            matches!(kill().await, CommandResult::Error { message, .. } if message.contains("locked"))
        );
        assert!(
            state.with(|s| s.resource_from_wire(&wire_pane).is_some()),
            "nothing was killed"
        );

        committed(handle(&state, CLIENT, lock(2, false), &root));
        assert_eq!(kill().await, CommandResult::Ok);
    });
}
