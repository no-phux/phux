use super::*;
#[cfg(feature = "engine")]
mod bounded_selection;
#[cfg(feature = "engine")]
mod views;
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
    attach_many_sized(owner, terminals, (20, 4));
}

fn attach_many_sized(
    owner: &EngineHandle,
    terminals: &[(&ResourceId, &[u8])],
    geometry: (u16, u16),
) {
    attach_many_history(owner, terminals, geometry, None);
}

fn attach_many_history(
    owner: &EngineHandle,
    terminals: &[(&ResourceId, &[u8])],
    geometry: (u16, u16),
    history_cursor: Option<&[u8]>,
) {
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
                cols: geometry.0,
                rows: geometry.1,
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
                payload: bytes.to_vec().into(),
            },
        );
        apply_ok(
            owner,
            EngineEvent::BootstrapReady {
                terminal_id: (*terminal_id).clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                history_cursor: history_cursor.map(<[u8]>::to_vec),
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
            bytes: b"\x1b[3;1Hthird".to_vec().into(),
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
    // A caught-up consumer: the batch publishes before anyone asks again.
    let before = publication
        .acquire(&terminal)
        .expect("published")
        .generation;
    let events = vec![
        EngineEvent::Output {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            seq: 1,
            bytes: b"\x1b[1;1Htop".to_vec().into(),
        },
        EngineEvent::Output {
            terminal_id: terminal.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            seq: 2,
            bytes: b"\x1b[3;1Hthird".to_vec().into(),
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
fn output(terminal: &ResourceId, seq: u64, bytes: &[u8]) -> EngineEvent {
    EngineEvent::Output {
        terminal_id: terminal.clone(),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        seq,
        bytes: bytes.to_vec().into(),
    }
}

#[cfg(feature = "engine")]
#[test]
fn an_unread_frame_defers_projection_until_the_next_acquire() {
    let (owner, publication) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"ready");
    let slot = publication.slot(&terminal).expect("published");
    let first = slot.generation();

    // Nobody read the attach frame: three batches replace nothing.
    apply_batch_ok(&owner, vec![output(&terminal, 1, b"\x1b[1;1Htop")]);
    apply_batch_ok(&owner, vec![output(&terminal, 2, b"\x1b[2;1Hsecond")]);
    apply_batch_ok(&owner, vec![output(&terminal, 3, b"\x1b[3;1Hthird")]);
    assert_eq!(slot.generation(), first, "unread frames are not replaced");

    // The read pulls one projection of the latest state, with the damage of
    // every deferred batch.
    let caught_up = slot.acquire().expect("caught up");
    assert_eq!(caught_up.generation, first + 1);
    assert_eq!(caught_up.last_seq, 3);
    assert_eq!(caught_up.row_text(0), "topdy");
    assert_eq!(caught_up.row_text(2), "third");
    assert_eq!(caught_up.damage, GridDamage::Rows);
    assert_eq!(caught_up.dirty_rows().collect::<Vec<_>>(), vec![0, 1, 2]);
    // Nothing changed since: a second read is the same frame, no projection.
    let again = slot.acquire().expect("current");
    assert!(Arc::ptr_eq(&caught_up, &again));

    // Read, so the next batch publishes at once: an echo is never held.
    apply_batch_ok(&owner, vec![output(&terminal, 4, b"\x1b[4;1Hecho")]);
    assert_eq!(slot.generation(), first + 2);
    let echo = publication.acquire(&terminal).expect("published");
    assert_eq!(echo.row_text(3), "echo");
    assert_eq!(echo.damage, GridDamage::Rows);
    assert!(echo.is_row_dirty(3));
    assert!(!echo.is_row_dirty(0));
}

/// The read/stale handshake under contention: a consumer that only
/// re-acquires when the generation moves must still end on the final state,
/// however its reads interleave with the owner's publish-or-defer choice.
#[cfg(feature = "engine")]
#[test]
fn a_generation_polling_consumer_never_strands_on_a_deferred_frame() {
    const BATCHES: u64 = 2_000;
    let (owner, publication) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"ready");
    let slot = publication.slot(&terminal).expect("published");
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let consumer = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut painted = slot.acquire().expect("frame");
            loop {
                let finished = done.load(std::sync::atomic::Ordering::SeqCst);
                if slot.generation() != painted.generation {
                    painted = slot.acquire().expect("frame");
                } else if finished {
                    return painted;
                }
                std::thread::yield_now();
            }
        })
    };
    for seq in 1..=BATCHES {
        let text = format!("\x1b[1;1H{seq:06}");
        apply_batch_ok(&owner, vec![output(&terminal, seq, text.as_bytes())]);
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let painted = consumer.join().expect("consumer");
    assert_eq!(painted.last_seq, BATCHES);
    assert_eq!(painted.row_text(0), format!("{BATCHES:06}"));
}

#[cfg(feature = "engine")]
#[test]
fn a_catch_up_after_the_owner_stops_returns_the_last_frame() {
    let (owner, publication) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"ready");
    let slot = publication.slot(&terminal).expect("published");
    apply_batch_ok(&owner, vec![output(&terminal, 1, b"\x1b[1;1Htop")]);
    let held = slot.generation();
    drop(owner);
    // Retiring the owner removes the slot; a held handle neither blocks nor
    // resurrects it.
    for _ in 0..100 {
        if slot.acquire().is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(slot.acquire().is_none());
    assert_eq!(slot.generation(), held);
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
                bytes: b"gap".to_vec().into(),
            },
            EngineEvent::Output {
                terminal_id: second.clone(),
                stream_id: stream(1),
                bootstrap_id: bootstrap(1),
                seq: 1,
                bytes: b"late".to_vec().into(),
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
            payload: b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix"
                .to_vec()
                .into(),
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
                bytes: b"stale".to_vec().into(),
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
            bytes: b"more".to_vec().into(),
        },
    );
    assert_eq!(owner.take_output(&terminal), b"more");
    assert!(owner.take_output(&terminal).is_empty());
}

#[cfg(feature = "engine")]
fn assert_search_projection_unavailable(owner: &EngineHandle, terminal: &ResourceId, anchor: u64) {
    for bound in [8, 0] {
        assert!(matches!(
            owner.search_bounded(terminal, "needle".into(), true, bound),
            Err(EngineError::ProjectionUnavailable)
        ));
    }
    assert!(matches!(
        owner.clear_search(terminal),
        Err(EngineError::ProjectionUnavailable)
    ));
    for handle in [u64::MAX, anchor] {
        assert!(matches!(
            owner.pin_viewport(terminal, handle),
            Err(EngineError::ProjectionUnavailable)
        ));
    }
}

#[cfg(feature = "engine")]
#[test]
fn missing_search_projection_is_typed_and_preserves_sibling_state() {
    let (owner, publication) = owner();
    let sibling = id(1);
    attach(
        &owner,
        &sibling,
        b"needle one\r\na\r\nb\r\nc\r\nneedle two\r\nd\r\ne\r\nf",
    );
    let found = owner.search(&sibling, "needle".into(), true).unwrap();
    owner.pin_viewport(&sibling, found[0].start).unwrap();
    let frame = publication.acquire(&sibling).unwrap();
    assert_search_projection_unavailable(&owner, &id(999), found[1].start);
    assert!(Arc::ptr_eq(&frame, &publication.acquire(&sibling).unwrap()));
    owner.pin_viewport(&sibling, found[1].start).unwrap();
    assert_eq!(
        publication.acquire(&sibling).unwrap().row_text(0),
        "needle two"
    );
}

#[cfg(feature = "engine")]
#[test]
fn detached_search_projection_is_unavailable_before_anchor_lookup() {
    let (owner, _) = owner();
    let terminal = id(1);
    let sibling = id(2);
    attach_many(&owner, &[(&terminal, b"needle"), (&sibling, b"needle")]);
    let old = owner.search(&terminal, "needle".into(), true).unwrap();
    let live = owner.search(&sibling, "needle".into(), true).unwrap();
    assert!(owner.detach(terminal.clone()));
    assert_search_projection_unavailable(&owner, &terminal, old[0].start);
    assert_search_projection_unavailable(&owner, &terminal, live[0].start);
    owner.pin_viewport(&sibling, live[0].start).unwrap();
}

#[cfg(feature = "engine")]
#[test]
fn non_retained_closed_search_projection_is_unavailable_before_anchor_lookup() {
    let (owner, _) = owner();
    let terminal = id(1);
    let sibling = id(2);
    attach_many(&owner, &[(&terminal, b"needle"), (&sibling, b"needle")]);
    let old = owner.search(&terminal, "needle".into(), true).unwrap();
    let live = owner.search(&sibling, "needle".into(), true).unwrap();
    apply_ok(&owner, EngineEvent::closed_unknown(terminal.clone()));
    assert_search_projection_unavailable(&owner, &terminal, old[0].start);
    assert_search_projection_unavailable(&owner, &terminal, live[0].start);
    owner.pin_viewport(&sibling, live[0].start).unwrap();
}

#[cfg(feature = "engine")]
#[test]
fn live_search_preserves_stale_and_wrong_owner_errors() {
    let (owner, _) = owner();
    let terminal = id(1);
    let sibling = id(2);
    attach_many(&owner, &[(&terminal, b"needle"), (&sibling, b"needle")]);
    let old = owner.search(&terminal, "needle".into(), true).unwrap();
    let fresh = owner.search(&terminal, "needle".into(), true).unwrap();
    assert!(matches!(
        owner.pin_viewport(&terminal, old[0].start),
        Err(EngineError::AnchorUnavailable(_))
    ));
    assert!(matches!(
        owner.pin_viewport(&sibling, fresh[0].start),
        Err(EngineError::Engine(_))
    ));
    let view = owner.create_view(&terminal).unwrap();
    let in_view = owner.search_view(view, "needle".into(), true).unwrap();
    assert!(matches!(
        owner.pin_viewport(&terminal, in_view[0].start),
        Err(EngineError::Engine(_))
    ));
    owner.clear_search(&terminal).unwrap();
    assert!(matches!(
        owner.pin_viewport(&terminal, fresh[0].start),
        Err(EngineError::AnchorUnavailable(_))
    ));
    let rebuilt = owner.search(&terminal, "needle".into(), true).unwrap();
    owner.clear_presentation(&terminal, 1, 1).unwrap();
    assert!(matches!(
        owner.pin_viewport(&terminal, rebuilt[0].start),
        Err(EngineError::AnchorUnavailable(_))
    ));
}

#[cfg(feature = "engine")]
#[test]
fn empty_search_query_precedes_projection_availability() {
    let (owner, _) = owner();
    assert!(matches!(
        owner.search_bounded(&id(999), String::new(), true, 0),
        Err(EngineError::Engine(reason)) if reason == "search query is empty"
    ));
}

#[cfg(feature = "engine")]
#[test]
fn retained_close_keeps_existing_kernel_search_refusal() {
    let (owner, _) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"needle");
    owner.set_retain_on_close(&terminal, true);
    apply_ok(&owner, EngineEvent::closed_unknown(terminal.clone()));
    assert!(owner.has_projection(&terminal));
    assert!(matches!(
        owner.search(&terminal, "needle".into(), true),
        Err(EngineError::Engine(_))
    ));
    owner.release(&terminal);
    assert_search_projection_unavailable(&owner, &terminal, u64::MAX);
}

#[cfg(feature = "engine")]
fn grey_theme(base: u8) -> TerminalTheme {
    TerminalTheme {
        ansi16: std::array::from_fn(|i| [base + u8::try_from(i).expect("16 entries"); 3]),
        foreground: [250; 3],
        background: [base; 3],
        cursor: [200; 3],
    }
}

/// ADR-0157: a theme set before attach colours the first frame, an
/// application's OSC 4 in the stream still wins for its entry, and a live
/// theme change republishes the visible terminal with the new palette.
#[cfg(feature = "engine")]
#[test]
fn the_terminal_theme_colours_published_frames_and_follows_live_changes() {
    let (owner, publication) = owner();
    owner.set_terminal_theme(Some(grey_theme(100)));
    let terminal = id(1);
    attach(&owner, &terminal, b"\x1b]4;3;rgb:03/03/03\x1b\\themed");
    let first = publication.acquire(&terminal).expect("published");
    let rgb = |c: crate::publication::Rgb| [c.r, c.g, c.b];
    assert_eq!(rgb(first.colors.palette[1]), [101; 3]);
    assert_eq!(rgb(first.colors.palette[3]), [3; 3], "the app's OSC 4 wins");
    assert_eq!(rgb(first.colors.background), [100; 3]);
    assert!(first.colors.has_background);

    owner.set_terminal_theme(Some(grey_theme(40)));
    let second = publication.acquire(&terminal).expect("republished");
    assert!(second.generation > first.generation);
    assert_eq!(rgb(second.colors.palette[1]), [41; 3]);
    assert_eq!(rgb(second.colors.palette[3]), [3; 3]);
    assert_eq!(rgb(second.colors.background), [40; 3]);

    owner.set_terminal_theme(None);
    let cleared = publication.acquire(&terminal).expect("republished");
    assert!(
        !cleared.colors.has_background,
        "clearing hands defaults back to the renderer"
    );
}
