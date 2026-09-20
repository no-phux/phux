use super::*;
#[cfg(feature = "engine")]
use crate::publication::GridDamage;

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

fn attach(owner: &EngineHandle, terminal_id: &ResourceId, bytes: &[u8]) {
    apply_ok(
        owner,
        EngineEvent::AttachStarted {
            attach_id: 7,
            terminals: vec![terminal_id.clone()],
        },
    );
    apply_ok(
        owner,
        EngineEvent::BootstrapBegin {
            terminal_id: terminal_id.clone(),
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
            terminal_id: terminal_id.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            chunk_seq: 0,
            payload: bytes.to_vec(),
        },
    );
    apply_ok(
        owner,
        EngineEvent::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            history_cursor: None,
        },
    );
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
