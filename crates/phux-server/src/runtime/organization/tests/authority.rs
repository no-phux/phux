//! Authority: extracted from the shared module.
use super::*;

/// A `resource:` grant names the record, not an activation: it admits
/// the relaunch of a dormant record and the live handle that relaunch
/// starts, which no `terminal:` id could have named in advance.
#[test]
pub(super) fn a_resource_key_grant_reaches_the_activation_a_relaunch_starts() {
    use phux_protocol::scope::TerminalScopeSet;
    use phux_protocol::wire::frame::LaunchMode;

    run(async {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let (session, _) = state.with_mut(|s| s.seed_session("work"));
        let session =
            state.with(|s| ow::session_key(s.registry().session_key(session).unwrap()).unwrap());
        let create = Command::ResourceCreate {
            placement: None,
            operation: op(1),
            session,
            kind: phux_protocol::ids::ResourceKind::Terminal,
            launch: Some(Box::new(LaunchSpec::new(vec!["sh".to_owned()]))),
            name: None,
            on_exit: phux_protocol::wire::frame::ExitPolicy::Remove,
            launch_mode: LaunchMode::Deferred,
        };
        let resource = committed(handle(&state, CLIENT, create, &root))
            .resource
            .expect("a record");
        let grant = crate::policy::ConnectionGrant::scoped(
            TerminalScopeSet::parse_all(&[&format!("*@resource:{resource}")]).unwrap(),
            Some("sha256:test".to_owned()),
        )
        .unwrap();
        let allowed = |s: &crate::state::ServerState, command: &Command| {
            crate::policy::enforce(s, CLIENT, &grant, crate::policy::Request::Command(command))
                .is_ok()
        };
        let record = page(&state, OrgFilter::Session(session))
            .resources
            .into_iter()
            .find(|r| r.key == resource)
            .expect("listed");
        let relaunch = Command::ResourceRelaunch {
            operation: op(2),
            resource,
            expected: record.revision,
        };
        assert!(state.with(|s| allowed(s, &relaunch)));
        assert!(matches!(
            handle(&state, CLIENT, relaunch, &root),
            CommandResult::Accepted { .. }
        ));
        let (live, other) = state.with_mut(|s| {
            let key = phux_core::ResourceKey::from_bytes(*resource.as_bytes());
            let id = s.registry().resource_id(&key).unwrap();
            let (_, sibling) = s.seed_session("elsewhere");
            (s.intern_terminal_wire(id), s.intern_terminal_wire(sibling))
        });
        let screen = |terminal_id| Command::GetScreen {
            terminal_id,
            request_scrollback: None,
            cells: false,
            format: 0,
        };
        state.with(|s| {
            assert!(allowed(s, &screen(live)), "the new activation's handle");
            assert!(!allowed(s, &screen(other)), "another record's handle");
        });
    });
}

#[test]
pub(super) fn arrangements_read_and_replace_by_revision() {
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
pub(super) fn a_stale_arrangement_replace_is_logged() {
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

#[test]
pub(super) fn terminate_is_accepted_and_a_stale_target_refused() {
    run(async {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let (_, pane) = state.with_mut(|s| s.seed_session("work"));
        let target = |activation| LiveTarget {
            resource: state.with_mut(|s| s.intern_terminal_wire(pane)),
            instance: state.with(|s| s.idspace.instance()),
            activation: phux_protocol::ids::ActivationId::new(activation),
        };
        let terminate = |n, activation| Command::ResourceTerminate {
            operation: op(n),
            target: target(activation),
            mode: TerminateMode::Graceful,
        };
        assert!(matches!(
            handle(&state, CLIENT, terminate(1, 7), &root),
            CommandResult::Refused(WireRefusal::Stale { .. })
        ));
        assert_eq!(
            handle(&state, CLIENT, terminate(2, 1), &root),
            CommandResult::Accepted { operation: op(2) }
        );
        let record = state.with(|s| s.registry().record(pane).cloned()).unwrap();
        assert_eq!(
            record.activation.unwrap().state,
            phux_core::ExecState::Stopping,
            "the outcome follows as ActivationChanged"
        );
    });
}

#[test]
pub(super) fn organization_events_are_journaled_with_their_actor() {
    run(async {
        let state = SharedState::new();
        state.with_mut(|s| s.set_client_name(CLIENT, "agent".to_owned()));
        let before = state.with(ServerState::journal_head);
        let create = Command::SessionCreate {
            operation: op(1),
            name: "work".to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: Vec::new(),
            launch: None,
        };
        let _ = committed(handle(&state, CLIENT, create, &CancellationToken::new()));
        let after = state.with(ServerState::journal_head);
        assert_eq!(
            after,
            before + 2,
            "SessionCreated and its ArrangementChanged"
        );
    });
}
