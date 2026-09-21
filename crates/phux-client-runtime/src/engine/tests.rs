use super::*;
#[cfg(feature = "engine")]
use crate::publication::GridDamage;
#[cfg(feature = "engine")]
use phux_client_core::session::KernelSend;

fn id(value: u32) -> ResourceId {
    ResourceId::local(value)
}

fn stream(value: u64) -> StreamId {
    StreamId::new(value).expect("nonzero")
}

fn bootstrap(value: u64) -> BootstrapId {
    BootstrapId::new(value).expect("nonzero")
}

fn config() -> EngineConfig {
    EngineConfig {
        profile: BootstrapProfile::SynthesizedVtRaw,
        limits: BootstrapLimits::default(),
        scrollback_lines: 100,
        history: None,
    }
}

#[cfg(feature = "engine")]
fn owner() -> (EngineHandle, Arc<Publication>) {
    let publication = Arc::new(Publication::new());
    let handle = EngineHandle::start(&config(), Arc::clone(&publication)).expect("owner");
    (handle, publication)
}

#[cfg(not(feature = "engine"))]
fn owner() -> EngineHandle {
    EngineHandle::start(&config()).expect("owner")
}

fn apply_ok(owner: &EngineHandle, event: EngineEvent) -> EngineOutcome {
    let outcome = owner.apply(event).expect("owner response");
    assert_eq!(outcome.error, None);
    assert!(!outcome.resync_required());
    outcome
}

#[cfg(feature = "engine")]
fn apply_batch_ok(owner: &EngineHandle, events: Vec<EngineEvent>) -> Vec<EngineOutcome> {
    let outcomes = owner.apply_batch(events).expect("owner response");
    assert!(outcomes.iter().all(|outcome| outcome.error.is_none()));
    assert!(outcomes.iter().all(|outcome| !outcome.resync_required()));
    outcomes
}

fn attach(owner: &EngineHandle, terminal_id: &ResourceId, bytes: &[u8]) {
    attach_many(owner, &[(terminal_id, bytes)]);
}

fn attach_many(owner: &EngineHandle, terminals: &[(&ResourceId, &[u8])]) {
    apply_ok(
        owner,
        EngineEvent::AttachStarted {
            attach_id: 7,
            terminals: terminals.iter().map(|(id, _)| (*id).clone()).collect(),
        },
    );
    for (terminal_id, bytes) in terminals {
        apply_ok(
            owner,
            EngineEvent::BootstrapBegin {
                terminal_id: (*terminal_id).clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 20,
                rows: 4,
                base_seq: 0,
            },
        );
        apply_ok(
            owner,
            EngineEvent::BootstrapChunk {
                terminal_id: (*terminal_id).clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                chunk_seq: 0,
                payload: bytes.to_vec(),
            },
        );
        apply_ok(
            owner,
            EngineEvent::BootstrapReady {
                terminal_id: (*terminal_id).clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                history_cursor: None,
            },
        );
    }
    apply_ok(owner, EngineEvent::AttachReady { attach_id: 7 });
}

#[cfg(feature = "engine")]
#[test]
fn a_published_replica_is_projected_and_generations_advance_on_output() {
    let (owner, publication) = owner();
    let terminal = id(1);
    assert!(!owner.has_projection(&terminal));
    attach(&owner, &terminal, b"ready");
    assert!(owner.has_projection(&terminal));
    assert!(owner.input_ready(&terminal));
    let first = publication.acquire(&terminal).expect("published");
    assert_eq!(first.generation, 1);
    assert_eq!(first.text(), "ready");
    assert_eq!(first.damage, GridDamage::Full);
    assert_eq!(first.dirty_rows().count(), 4, "a first frame is all dirty");

    // Nothing applied: the generation does not move.
    assert_eq!(publication.generation(&terminal), Some(1));

    apply_ok(
        &owner,
        EngineEvent::Output {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            seq: 1,
            bytes: b"\x1b[3;1Hthird".to_vec(),
        },
    );
    let second = publication.acquire(&terminal).expect("published");
    assert_eq!(second.generation, 2);
    assert_eq!(second.row_text(2), "third");
    assert!(second.is_row_dirty(2));
    assert_eq!(first.text(), "ready", "the held frame is untouched");

    // A scroll re-publishes even when the grid did not change.
    owner.scroll(&terminal, Scroll::Bottom).expect("scroll");
    assert_eq!(publication.generation(&terminal), Some(3));
}

#[cfg(feature = "engine")]
#[test]
fn an_output_burst_projects_once_without_losing_event_outcomes() {
    let (owner, publication) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"ready");
    let before = publication.generation(&terminal).expect("published");
    let events = vec![
        EngineEvent::Output {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            seq: 1,
            bytes: b"\x1b[1;1Htop".to_vec(),
        },
        EngineEvent::Output {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            seq: 2,
            bytes: b"\x1b[3;1Hthird".to_vec(),
        },
    ];

    let outcomes = apply_batch_ok(&owner, events);

    assert_eq!(outcomes.len(), 2);
    assert_eq!(publication.generation(&terminal), Some(before + 1));
    assert_eq!(owner.replica_info(&terminal).expect("replica").last_seq, 2);
    let frame = publication.acquire(&terminal).expect("published");
    assert_eq!(frame.row_text(0), "topdy");
    assert_eq!(frame.row_text(2), "third");
    assert_eq!(frame.damage, GridDamage::Rows);
    assert_eq!(
        frame.dirty_rows().collect::<Vec<_>>(),
        vec![0, 2],
        "one final projection preserves the burst's exact accumulated row damage"
    );
}

#[cfg(feature = "engine")]
#[test]
fn a_fatal_event_stops_the_batch_before_later_terminals_mutate() {
    let (owner, publication) = owner();
    let first = id(1);
    let second = id(2);
    attach_many(&owner, &[(&first, b"first"), (&second, b"second")]);
    let second_generation = publication.generation(&second).expect("second published");

    let outcomes = owner
        .apply_batch(vec![
            EngineEvent::Output {
                terminal_id: first,
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                seq: 2,
                bytes: b"gap".to_vec(),
            },
            EngineEvent::Output {
                terminal_id: second.clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                seq: 1,
                bytes: b"late".to_vec(),
            },
        ])
        .expect("owner response");

    assert_eq!(
        outcomes.len(),
        1,
        "the fatal outcome ends the applied prefix"
    );
    assert!(matches!(
        outcomes[0].error,
        Some(EngineApplyError::Protocol(_))
    ));
    assert_eq!(
        owner
            .replica_info(&second)
            .expect("second replica")
            .last_seq,
        0
    );
    assert_eq!(publication.generation(&second), Some(second_generation));
    assert_eq!(
        publication.acquire(&second).expect("second frame").text(),
        "second"
    );
}

#[cfg(feature = "engine")]
#[test]
fn scrolling_toward_uncached_history_emits_a_prefetch_request() {
    let publication = Arc::new(Publication::new());
    let mut history_config = config();
    history_config.history = Some(HistoryCacheConfig::default());
    let owner = EngineHandle::start(&history_config, publication).expect("owner");
    let terminal = id(9);
    apply_ok(
        &owner,
        EngineEvent::AttachStarted {
            attach_id: 7,
            terminals: vec![terminal.clone()],
        },
    );
    apply_ok(
        &owner,
        EngineEvent::BootstrapBegin {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        },
    );
    apply_ok(
        &owner,
        EngineEvent::BootstrapChunk {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            chunk_seq: 0,
            payload: b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix".to_vec(),
        },
    );
    apply_ok(
        &owner,
        EngineEvent::BootstrapReady {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            history_cursor: Some(b"older".to_vec()),
        },
    );
    apply_ok(&owner, EngineEvent::AttachReady { attach_id: 7 });
    apply_ok(
        &owner,
        EngineEvent::HistoryRejected {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            cursor: b"older".to_vec(),
            reason: HistoryRejectionReason::ZeroLimit,
            required_bytes: 0,
            required_rows: 0,
        },
    );

    let outcome = owner.scroll(&terminal, Scroll::Top).expect("scroll");
    assert!(outcome.effects.iter().any(|effect| matches!(
        effect,
        KernelEffect::Send(KernelSend::HistoryRequest { cursor, .. }) if cursor == b"older"
    )));
}

#[cfg(feature = "engine")]
#[test]
fn a_closed_terminal_loses_its_projection_unless_retained() {
    let (owner, publication) = owner();
    let terminal = id(2);
    attach(&owner, &terminal, b"final");
    owner.set_retain_on_close(&terminal, true);
    apply_ok(&owner, EngineEvent::closed_unknown(terminal.clone()));
    assert!(owner.is_closed(&terminal));
    assert!(owner.has_projection(&terminal), "retained after close");
    assert!(publication.acquire(&terminal).is_some());
    owner.release(&terminal);
    assert!(!owner.has_projection(&terminal));
    assert!(publication.acquire(&terminal).is_none());
}

#[cfg(feature = "engine")]
#[test]
fn a_frame_queued_after_close_cannot_recreate_the_projection() {
    let (owner, publication) = owner();
    let terminal = id(3);
    attach(&owner, &terminal, b"final");

    let outcomes = apply_batch_ok(
        &owner,
        vec![
            EngineEvent::closed_unknown(terminal.clone()),
            EngineEvent::Output {
                terminal_id: terminal.clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                seq: 1,
                bytes: b"stale".to_vec(),
            },
        ],
    );

    assert_eq!(outcomes.len(), 2);
    assert!(outcomes[1].effects.is_empty());
    assert!(owner.is_closed(&terminal));
    assert!(!owner.has_projection(&terminal));
    assert!(publication.acquire(&terminal).is_none());
}

#[cfg(feature = "engine")]
#[test]
fn releasing_a_live_terminal_keeps_its_current_publication() {
    let (owner, publication) = owner();
    let terminal = id(3);
    attach(&owner, &terminal, b"live");
    let generation = publication.generation(&terminal);

    owner.release(&terminal);

    assert!(owner.has_projection(&terminal));
    assert_eq!(publication.generation(&terminal), generation);
}

#[cfg(not(feature = "engine"))]
#[test]
fn the_headless_replica_keeps_bytes_until_taken() {
    let owner = owner();
    let terminal = id(3);
    attach(&owner, &terminal, b"ready");
    assert!(owner.has_projection(&terminal));
    assert_eq!(owner.take_output(&terminal), b"ready");
    apply_ok(
        &owner,
        EngineEvent::Output {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            seq: 1,
            bytes: b"more".to_vec(),
        },
    );
    assert_eq!(owner.take_output(&terminal), b"more");
    assert!(owner.take_output(&terminal).is_empty());
}
