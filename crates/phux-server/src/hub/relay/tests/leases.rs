//! Relayed events through the hub registry, and the satellite input lease mirror.

use super::*;

#[test]
fn relayed_events_go_through_the_hub_registry_with_the_hubs_seq() {
    let journal = crate::state::SharedState::new();
    let consumer = journal.with_mut(crate::state::ServerState::new_client_id);
    let (registry_tx, mut registry_rx) = mpsc::channel(8);
    journal.with_mut(|s| {
        s.subscribe_satellite_events(
            consumer,
            ResourceId::satellite("devbox", 9),
            crate::state::EventFilter::all(),
            None,
            registry_tx,
        );
    });
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    session.set_journal(Some(journal.clone()));
    let (proxy_tx, mut proxy_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, consumer, proxy_tx);
    let actor = phux_protocol::wire::frame::ActorRef::new(phux_protocol::ids::ClientId::new(4))
        .with_client_name(Some("satellite-side".to_owned()));
    let satellite_stamp =
        phux_protocol::wire::frame::EventStamp::new(77, 1_234).with_actor(Some(actor));
    let scoped_gap = AgentEvent::JournalGap {
        first_missing: 5,
        last_missing: 7,
    };
    let link_gap = AgentEvent::JournalGap {
        first_missing: 1,
        last_missing: 4,
    };
    for (terminal, event, stamp) in [
        (
            Some(ResourceId::local(9)),
            AgentEvent::CommandStarted,
            Some(Box::new(satellite_stamp)),
        ),
        (Some(ResourceId::local(9)), scoped_gap, None),
        (None, link_gap, None),
    ] {
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal,
                event,
                stamp,
            }))
            .expect("valid satellite frame");
    }
    let delivered: Vec<_> = std::iter::from_fn(|| registry_rx.try_recv().ok())
        .map(|out| match out {
            Outbound::Frame(FrameKind::Event {
                terminal,
                event,
                stamp,
            }) => (terminal, event, stamp.expect("stamped by the hub")),
            other => panic!("expected an EVENT, got {other:?}"),
        })
        .collect();
    let [(t1, started, first), (t2, gap, second), (t3, link, third)] = delivered.as_slice() else {
        panic!("three events through the registry: {delivered:?}");
    };
    let scope = Some(ResourceId::satellite("devbox", 9));
    assert!([t1, t2, t3].iter().all(|t| **t == scope), "re-tagged");
    assert_eq!(*started, AgentEvent::CommandStarted);
    assert_eq!(first.seq, 1, "the hub's seq, not the satellite's 77");
    assert_eq!(first.ts_ms, 1_234, "the satellite's time crosses the hub");
    assert_eq!(first.actor, None, "the satellite's actor does not");
    assert_eq!(*gap, AgentEvent::SourceGap { dropped: 3 });
    assert_eq!(*link, AgentEvent::SourceGap { dropped: 4 });
    assert_eq!((second.seq, third.seq), (2, 3));
    assert!(
        proxy_rx.try_recv().is_err(),
        "events are delivered once, by the registry, not by the relay"
    );
    assert_eq!(journal.with(crate::state::ServerState::journal_head), 3);
}

// --- lease TTL mirror (ADR-0033) ---

/// A satellite `EXPIRED`/`RELEASED` frees the hub's ledger entry.
#[test]
fn a_satellite_expiry_or_release_clears_the_hubs_satellite_lease() {
    for action in [ControlAction::Expired, ControlAction::Released] {
        let journal = crate::state::SharedState::new();
        let holder = journal.with_mut(crate::state::ServerState::new_client_id);
        let (mailbox_tx, _mailbox_rx) = mpsc::channel(1);
        journal.with_mut(|s| {
            s.set_satellite_lease(host(), 9, holder, mailbox_tx.clone());
        });
        assert_eq!(
            journal.with(|s| s.satellite_lease_holder(&host(), 9)),
            Some(holder),
            "{action:?}: precondition"
        );
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        session.set_journal(Some(journal.clone()));
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::TerminalControl {
                    lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
                    exit_status: None,
                    input_holder: None,
                    action,
                    actor: None,
                },
                stamp: None,
            }))
            .expect("valid satellite frame");
        assert_eq!(
            journal.with(|s| s.satellite_lease_holder(&host(), 9)),
            None,
            "{action:?}: the hub ledger must follow the satellite"
        );
    }
}

/// A `SEIZED` is not a free lease; the ledger is left alone.
#[test]
fn a_satellite_seize_does_not_clear_the_hubs_satellite_lease() {
    let journal = crate::state::SharedState::new();
    let holder = journal.with_mut(crate::state::ServerState::new_client_id);
    let (mailbox_tx, _mailbox_rx) = mpsc::channel(1);
    journal.with_mut(|s| {
        s.set_satellite_lease(host(), 9, holder, mailbox_tx);
    });
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    session.set_journal(Some(journal.clone()));
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::TerminalControl {
                lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
                exit_status: None,
                input_holder: Some(phux_protocol::ids::ClientId::new(99)),
                action: ControlAction::Seized,
                actor: None,
            },
            stamp: None,
        }))
        .expect("valid satellite frame");
    assert_eq!(
        journal.with(|s| s.satellite_lease_holder(&host(), 9)),
        Some(holder),
        "a SEIZED mirror must not evict the hub ledger"
    );
}

/// A stale Released that arrives after a newer acquire's reply must not
/// evict the new holder.
#[test]
fn a_stale_released_arriving_after_a_newer_acquires_reply_does_not_evict_it() {
    let journal = crate::state::SharedState::new();
    let a = journal.with_mut(crate::state::ServerState::new_client_id);
    let b = journal.with_mut(crate::state::ServerState::new_client_id);
    let (a_tx, _a_rx) = mpsc::channel(1);
    let (b_tx, _b_rx) = mpsc::channel(1);

    // A held the lease.
    journal.with_mut(|s| {
        s.set_satellite_lease(host(), 9, a, a_tx);
    });

    // B's acquire relay marks pending...
    journal.with_mut(|s| s.mark_satellite_lease_acquire_pending(host(), 9));
    // ...and its reply lands first; only the event mirror clears the mark.
    journal.with_mut(|s| {
        s.set_satellite_lease(host(), 9, b, b_tx);
    });
    assert_eq!(
        journal.with(|s| s.satellite_lease_holder(&host(), 9)),
        Some(b),
        "precondition: the reply already installed B"
    );

    // A's delayed Released arrives.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    session.set_journal(Some(journal.clone()));
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::TerminalControl {
                lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
                exit_status: None,
                input_holder: None,
                action: ControlAction::Released,
                actor: None,
            },
            stamp: None,
        }))
        .expect("valid satellite frame");

    assert_eq!(
        journal.with(|s| s.satellite_lease_holder(&host(), 9)),
        Some(b),
        "a stale Released arriving after a newer reply must not evict the holder it installed"
    );

    // The mark is consumed; a later Released applies normally.
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::TerminalControl {
                lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
                exit_status: None,
                input_holder: None,
                action: ControlAction::Released,
                actor: None,
            },
            stamp: None,
        }))
        .expect("valid satellite frame");
    assert_eq!(
        journal.with(|s| s.satellite_lease_holder(&host(), 9)),
        None,
        "a later, non-racing Released is honored normally"
    );
}

/// L1 §7.1 / ADR-0123: journal semantics on the link only when the
/// satellite speaks them.
#[test]
fn the_link_subscribes_events_with_journal_semantics_when_the_satellite_speaks_them() {
    for (features, cursor) in [
        (
            ServerFeatureSet::with(&[ServerFeature::EventJournal]),
            Some(u64::MAX),
        ),
        (ServerFeatureSet::new(), None),
    ] {
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            BootstrapProfile::SynthesizedVtRaw,
            features,
        );
        let (out_tx, _out_rx) = mpsc::channel(1);
        let wire = session.handle_request(RelayRequest::Subscribe {
            subscription: ProxySubscription {
                resource_kind: None,
                terminal: 9,
                client: ClientId(1),
                out_tx,
                consumer_cancel: CancellationToken::new(),
                seq: 1,
                awaits_snapshot: false,
                bootstrap_profile: None,
                bootstrap_limits: None,
            },
            forward: FrameKind::SubscribeEvents {
                terminal: Some(ResourceId::local(9)),
                after_seq: None,
            },
        });
        assert!(
            matches!(
                decode(&wire),
                FrameKind::SubscribeEvents { after_seq, .. } if after_seq == cursor
            ),
            "forwarded cursor must be {cursor:?}"
        );
    }
}

// --- session: return-leg re-tagging ----------------------------------
