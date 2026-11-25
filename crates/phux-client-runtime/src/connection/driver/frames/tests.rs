use std::sync::{Arc, Mutex};

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfileKind, BootstrapProfileSet,
    ServerCapabilities, select_bootstrap_profile,
};
use phux_protocol::ids::{BootstrapId, StreamId};
#[cfg(feature = "engine")]
use phux_protocol::caps::BootstrapStreamProfile;

use super::*;
use crate::control::frames::stepped::{BatchStep, EngineApply, FrameBatch};
use phux_protocol::ResourceId;
use crate::control::{ControlOptions, encode};

fn negotiated() -> ControlPlane {
    let mut plane = ControlPlane::new(std::time::Instant::now(), ControlOptions::default());
    negotiate(&mut plane);
    plane
}

fn negotiate(plane: &mut ControlPlane) {
    plane.connection_opened();
    let hello = plane.take_outbound();
    let (FrameKind::Hello { client_caps, .. }, _) =
        FrameKind::decode(&hello[0]).expect("HELLO decodes")
    else {
        panic!("the opening frame is HELLO");
    };
    let (profile, limits) =
        select_bootstrap_profile(
            &client_caps,
            &BootstrapCapabilities::new().with_profiles(BootstrapProfileSet::with(&[
                BootstrapProfileKind::SynthesizedVtRaw,
            ])),
        )
            .expect("intersect");
    plane
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: b"incarnation".to_vec(),
            selected_profile: Some(profile),
            bootstrap_limits: limits,
        })
        .expect("HELLO_OK");
    assert!(plane.engine().is_some(), "HELLO_OK starts the engine");
}

/// Output for a terminal this connection never bootstrapped: a run the
/// engine applies and answers with an error.
fn stray_output() -> Bytes {
    Bytes::from(encode(&FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(7),
        stream_id: StreamId::new(1).expect("stream"),
        bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
        seq: 1,
        bytes: b"late".to_vec().into(),
    }))
}

fn handed_out(shared: &Shared) -> (FrameBatch<std::iter::Once<Bytes>>, EngineApply) {
    let mut batch = FrameBatch::new(std::iter::once(stray_output()), |plane, frame| {
        plane.decode_shared_frame(&frame)
    });
    let BatchStep::Apply(apply) = lock(shared).step_frame_batch(&mut batch) else {
        panic!("an engine run is handed out");
    };
    (batch, apply)
}

/// The point of stepping: while a run is with the engine, the plane is
/// free for every other reader.
#[test]
fn a_run_is_with_the_engine_while_the_plane_is_free() {
    let shared: Shared = Arc::new(Mutex::new(negotiated()));
    let (mut batch, apply) = handed_out(&shared);
    let drained = shared
        .try_lock()
        .map(|mut plane| plane.take_observation())
        .is_ok();
    assert!(drained, "observation drains without waiting on the engine");
    let outcomes = apply.engine.apply_batch(apply.events);
    let BatchStep::Done(_) =
        lock(&shared).complete_frame_apply(&mut batch, apply.geometry, outcomes)
    else {
        panic!("a one-frame read is done after its run");
    };
}

/// A consumer that closes while the engine applies a run keeps none of
/// it: the run belonged to a connection that is gone.
#[test]
fn a_close_during_the_apply_keeps_none_of_the_run() {
    let open: Shared = Arc::new(Mutex::new(negotiated()));
    let (mut batch, apply) = handed_out(&open);
    let outcomes = apply.engine.apply_batch(apply.events);
    let BatchStep::Done(still_open) =
        lock(&open).complete_frame_apply(&mut batch, apply.geometry, outcomes)
    else {
        panic!("done");
    };
    assert!(
        still_open.is_err(),
        "an open connection hears the stray run's error"
    );

    let closed: Shared = Arc::new(Mutex::new(negotiated()));
    let (mut batch, apply) = handed_out(&closed);
    lock(&closed).close();
    let _ = lock(&closed).take_events();
    let outcomes = apply.engine.apply_batch(apply.events);
    let BatchStep::Done(result) =
        lock(&closed).complete_frame_apply(&mut batch, apply.geometry, outcomes)
    else {
        panic!("done");
    };
    assert!(
        result.is_ok(),
        "a closed connection's run is discarded: {result:?}"
    );
    assert!(lock(&closed).take_events().is_empty(), "and emits nothing");
}

#[cfg(feature = "engine")]
#[test]
fn an_old_completion_cannot_reset_a_replacement_connections_published_frame() {
    let shared: Shared = Arc::new(Mutex::new(negotiated()));
    let (mut old_batch, apply) = handed_out(&shared);
    let outcomes = apply.engine.apply_batch(apply.events);
    {
        let mut plane = lock(&shared);
        plane.connection_lost(None);
        negotiate(&mut plane);
    }
    let terminal = ResourceId::local(19);
    let stream_id = StreamId::new(2).expect("stream");
    let bootstrap_id = BootstrapId::new(2).expect("bootstrap");
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: terminal.clone(), stream_id, bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20, rows: 4, base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal.clone(), stream_id, bootstrap_id,
            chunk_seq: 0, payload: Bytes::from_static(b"replacement"),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal.clone(), stream_id, bootstrap_id, history_cursor: None,
        },
    ] {
        feed_unlocked(&shared, frame).expect("replacement bootstrap");
    }
    let engine = lock(&shared).engine().expect("owner").clone();
    assert!(engine.has_projection(&terminal));
    let _ = lock(&shared).take_events();
    let BatchStep::Done(result) = lock(&shared).complete_frame_apply(
        &mut old_batch, apply.geometry, outcomes,
    ) else { panic!("retired batch cannot resume"); };
    assert!(result.is_ok(), "old errors cannot settle the replacement");
    assert!(engine.has_projection(&terminal), "old completion cannot reset the new owner state");
    assert!(lock(&shared).take_events().is_empty());
    assert_eq!(
        lock(&shared).publication().acquire(&terminal).expect("published").text(),
        "replacement",
    );
}

#[test]
fn native_pump_releases_control_while_the_actual_owner_batch_is_waiting() {
    let shared: Shared = Arc::new(Mutex::new(negotiated()));
    let engine = lock(&shared).engine().expect("owner").clone();
    let (entered, resume) = engine.pause_next_apply_for_test().expect("arm owner gate");
    let worker = std::thread::spawn({
        let shared = Arc::clone(&shared);
        move || feed_shared_unlocked(&shared, vec![stray_output()])
    });
    entered.recv_timeout(std::time::Duration::from_secs(5)).expect("actual owner apply");
    let observation_available = shared.try_lock().map(|mut plane| plane.take_observation()).is_ok();
    resume.send(()).expect("release owner");
    let result = worker.join().expect("native frame pump");
    assert!(result.is_err(), "the original engine error remains observable");
    assert!(observation_available, "consumer observations cannot wait for an engine batch");
}
