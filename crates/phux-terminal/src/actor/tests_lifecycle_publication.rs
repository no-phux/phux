//! Deterministic READY-to-activation interleavings against the real native actor.

use super::*;
use crate::native_state::NativeStateError;
use phux_protocol::ids::{BootstrapId, StreamId};

type Pump = (u64, StreamId);
type CaptureReceiver = oneshot::Receiver<Result<NativeBootstrapReply, NativeStateError>>;

fn pumps(same_owner: bool) -> [Pump; 2] {
    [
        (7, StreamId::new(1).expect("first stream")),
        (
            if same_owner { 7 } else { 8 },
            StreamId::new(2).expect("second stream"),
        ),
    ]
}

fn replacement_id() -> BootstrapId {
    BootstrapId::new(2).expect("replacement bootstrap")
}

/// Start from two activated generations, then join both owners to one capture.
fn begin_replacements(actor: &mut TerminalActor, pumps: [Pump; 2]) -> [CaptureReceiver; 2] {
    let initial = BootstrapId::new(1).expect("initial bootstrap");
    for (owner, stream) in pumps {
        let capture = capture_native_for_pump(actor, owner, stream, initial);
        activate_native_for_pump(actor, owner, stream, initial, capture.capture.cursor)
            .expect("initial publication");
    }
    let receivers = pumps.map(|(owner, stream)| {
        let (reply, response) = oneshot::channel();
        actor.handle_native_bootstrap(native_request(
            owner,
            phux_protocol::ids::ResourceId::local(1),
            (stream, replacement_id()),
            reply,
        ));
        response
    });
    assert_eq!(
        actor.publication
            .pending_native_bootstrap
            .as_ref()
            .map(|p| p.waiters.len()),
        Some(2),
        "both replacement pumps must join one real capture"
    );
    receivers
}

/// The existing actor fixtures also advance at most 64 cooperative engine steps.
fn finish_capture(
    actor: &mut TerminalActor,
    mut response: CaptureReceiver,
) -> NativeBootstrapReply {
    for _ in 0..64 {
        match response.try_recv() {
            Ok(reply) => {
                let mut capture = reply.expect("real native capture");
                capture.capture.published();
                return capture;
            }
            Err(oneshot::error::TryRecvError::Empty) => actor.cooperative_native_step(),
            Err(oneshot::error::TryRecvError::Closed) => panic!("capture reply dropped"),
        }
    }
    panic!("fixture capture did not finish within its cooperative step bound")
}

fn finish_captures(
    actor: &mut TerminalActor,
    receivers: [CaptureReceiver; 2],
) -> [NativeBootstrapReply; 2] {
    let captures = receivers.map(|response| finish_capture(actor, response));
    assert_eq!(
        captures[0].capture.cursor,
        captures[1].capture.cursor
    );
    assert!(!actor.native_bootstrap_pending());
    captures
}

fn activate_replacements(
    actor: &mut TerminalActor,
    pumps: [Pump; 2],
    captures: &[NativeBootstrapReply; 2],
) -> [Option<NativeStateError>; 2] {
    std::array::from_fn(|i| {
        let (owner, stream) = pumps[i];
        let result = activate_native_for_pump(
            actor,
            owner,
            stream,
            replacement_id(),
            captures[i].capture.cursor,
        );
        result.err()
    })
}

fn resize(cols: u16, rows: u16) -> ResizeRequest {
    ResizeRequest {
        cols,
        rows,
        cell_px: None,
        resync_clients: true,
        resync_only: false,
        resync_for: None,
    }
}

/// Pin the post-capture window separately from mid-capture invalidation.
/// A known resize must remain distinguishable from malformed publication identity.
#[tokio::test(flavor = "current_thread")]
async fn admitted_resize_after_ready_is_recoverable_for_both_replacement_owners() {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let terminal = bundle.handle.terminal().expect("terminal").clone();
    let mut actor = bundle.actor;
    let pumps = pumps(false);
    let receivers = begin_replacements(&mut actor, pumps);
    terminal
        .resize
        .try_send(resize(30, 8))
        .expect("admit geometry while capture is pending");
    assert!(actor.native_bootstrap_pending());
    let captures = finish_captures(&mut actor, receivers);

    // Neither owner has activated. The run loop admits this queued resize before
    // publication as soon as native_bootstrap_pending becomes false.
    let admitted = actor.resize_rx.try_recv().expect("queued geometry");
    let owed = actor.apply_resize_request(admitted);
    assert_eq!(owed.len(), 1, "the reflow must owe a replacement resync");
    assert_eq!(actor.publication.native_cursor_owners.len(), 2);
    assert!(
        actor.publication
            .native_cursor_owners
            .values()
            .all(|binding| binding.superseded)
    );
    assert!(actor.publication.native_publications.is_empty());
    let errors = activate_replacements(&mut actor, pumps, &captures);
    assert_eq!(
        errors,
        [Some(NativeStateError::Resize); 2],
        "known post-READY reflow must be recoverable, not an unclassified InvalidHandle"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unchanged_geometry_preserves_both_replacement_publications() {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let pumps = pumps(false);
    let receivers = begin_replacements(&mut actor, pumps);
    let captures = finish_captures(&mut actor, receivers);
    assert!(actor.apply_resize_request(resize(20, 5)).is_empty());
    assert_eq!(
        activate_replacements(&mut actor, pumps, &captures),
        [None; 2]
    );
}

fn finish_history_reply(
    actor: &mut TerminalActor,
    mut response: oneshot::Receiver<NativeHistoryReply>,
) -> FrameKind {
    for _ in 0..64 {
        match response.try_recv() {
            Ok(reply) => return reply.result.expect("native history frame"),
            Err(oneshot::error::TryRecvError::Empty) => actor.cooperative_native_step(),
            Err(oneshot::error::TryRecvError::Closed) => panic!("history reply dropped"),
        }
    }
    panic!("fixture history did not finish within its cooperative step bound")
}

/// Use the real client kernel's READY-triggered requests, including authenticated
/// FINISH, without granting the server publication request an earlier turn.
#[allow(
    clippy::future_not_send,
    reason = "real native actor and client kernel are LocalSet-only"
)]
async fn complete_history(
    actor: &mut TerminalActor,
    pump: Pump,
    capture: &NativeBootstrapReply,
) -> usize {
    use phux_client_core::engine::ghostty::GhosttyAdapter;
    use phux_client_core::history::HistoryLoadState;
    use phux_client_core::session::{EffectBuffer, SessionKernel};
    use phux_protocol::caps::{BootstrapProfile, EngineCodec, EngineFeatureSet};

    let limits = phux_protocol::caps::BootstrapLimits::default();
    let mut kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(limits),
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttySnapshotV1,
            features: EngineFeatureSet::required_native(),
        },
        phux_client_core::history::HistoryCacheConfig {
            max_materialized_rows: 1,
            prefetch_rows: 2,
            request_max_rows: 1024,
            ..phux_client_core::history::HistoryCacheConfig::default()
        },
    );
    let mut effects = EffectBuffer::new();
    for frame in &capture.frames {
        apply_native_wire_frame(&mut kernel, frame, &mut effects);
    }
    let terminal_id = phux_protocol::ids::ResourceId::local(1);
    assert!(kernel.published(&terminal_id).is_some(), "READY committed");
    let (outbound, _receiver) = mpsc::channel(1);
    let mut next = take_history_request(&mut effects);
    assert!(
        next.is_some(),
        "READY automatically requests native history"
    );
    let mut pages = 0;
    while let Some((cursor, max_bytes, max_rows)) = next {
        let permit = outbound
            .clone()
            .reserve_owned()
            .await
            .expect("history permit");
        let (reply, response) = oneshot::channel();
        actor.handle_native_history(NativeHistoryRequest {
            permit,
            owner: pump.0,
            terminal_id: terminal_id.clone(),
            stream_id: pump.1,
            bootstrap_id: replacement_id(),
            cursor: cursor.into(),
            max_bytes,
            max_rows,
            limits,
            reply,
        });
        let frame = finish_history_reply(actor, response);
        assert!(
            matches!(frame, FrameKind::HistoryPage { .. }),
            "authenticated history page"
        );
        apply_native_wire_frame(&mut kernel, &frame, &mut effects);
        next = take_history_request(&mut effects);
        pages += 1;
        assert!(pages <= 64, "bounded fixture history");
    }
    assert_eq!(
        kernel
            .history_cache(&terminal_id)
            .expect("history")
            .status()
            .state,
        HistoryLoadState::Complete,
        "the real authenticated history FINISH must have been consumed"
    );
    pages
}

#[path = "tests_lifecycle_publication_lifetime.rs"]
mod lifetime;

#[tokio::test(flavor = "current_thread")]
async fn normal_history_finish_before_activation_preserves_each_shared_owner() {
    let mut observations = Vec::new();
    for same_owner in [false, true] {
        let bundle = TerminalActor::new(20, 5).expect("actor");
        let mut actor = bundle.actor;
        let pumps = pumps(same_owner);
        let receivers = begin_replacements(&mut actor, pumps);
        let captures = finish_captures(&mut actor, receivers);
        for i in 0..2 {
            complete_history(&mut actor, pumps[i], &captures[i]).await;
        }
        observations.push(activate_replacements(&mut actor, pumps, &captures));
    }
    assert_eq!(
        observations,
        vec![[None; 2]; 2],
        "normal history completion must not destroy the pending replay/live handoff"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn finishing_one_shared_history_keeps_the_sibling_publication_usable() {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let pumps = pumps(false);
    let receivers = begin_replacements(&mut actor, pumps);
    let captures = finish_captures(&mut actor, receivers);
    complete_history(&mut actor, pumps[0], &captures[0]).await;
    let errors = activate_replacements(&mut actor, pumps, &captures);
    assert_eq!(
        errors[1], None,
        "the sibling's publication remains independent"
    );
    assert_eq!(
        errors[0], None,
        "FINISH must preserve its own publication too"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn releasing_the_old_owner_preserves_the_current_pending_publication() {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let pumps = pumps(false);
    let receivers = begin_replacements(&mut actor, pumps);
    let captures = finish_captures(&mut actor, receivers);
    actor.release_native_owner(pumps[0].0);
    activate_native_for_pump(
        &mut actor,
        pumps[1].0,
        pumps[1].1,
        replacement_id(),
        captures[1].capture.cursor,
    )
    .expect("old owner release must not invalidate current publication");
}

#[tokio::test(flavor = "current_thread")]
async fn wrong_bootstrap_identity_is_rejected_without_consuming_valid_publication() {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let pumps = pumps(false);
    let receivers = begin_replacements(&mut actor, pumps);
    let captures = finish_captures(&mut actor, receivers);
    let bad = activate_native_for_pump(
        &mut actor,
        pumps[0].0,
        pumps[0].1,
        BootstrapId::new(99).expect("wrong bootstrap"),
        captures[0].capture.cursor,
    );
    assert_eq!(bad.err(), Some(NativeStateError::InvalidHandle));
    assert_eq!(
        activate_replacements(&mut actor, pumps, &captures),
        [None; 2]
    );
}
