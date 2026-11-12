//! History failure remains isolated while a replacement replica is staged.

use super::*;

#[test]
fn malformed_history_tombstones_only_history_and_replacement_publishes_atomically() {
    let id = tid(96);
    let replacement = phux_protocol::BootstrapId::new(2).expect("replacement");
    let mut h = H::new();
    h.send(begin_frame(&id));
    h.send(chunk_frame(&id, b"\x1b]2;old\x07"));
    h.send(ready_frame(&id));

    assert!(!h.mirror.panes[&id].history_degraded);
    let rejected = h.send(history_page(&id, b"cursor"));
    assert!(!rejected.resync_required);
    assert!(rejected.chrome_dirty && h.mirror.panes[&id].history_degraded);
    assert!(
        h.mirror
            .kernel_effects
            .as_slice()
            .iter()
            .any(|effect| matches!(
                effect,
                phux_client_core::session::KernelEffect::Status(
                    phux_client_core::session::KernelStatus::HistoryUnavailable { .. }
                )
            ))
    );
    assert_eq!(
        h.mirror
            .engine_kernel
            .history_cache(&id)
            .expect("history")
            .status()
            .state,
        phux_client_core::history::HistoryLoadState::Tombstoned
    );
    h.send(output_frame(&id, 1, b"\x1b]2;old-live\x07"));
    let title = |h: &H| {
        h.mirror
            .engine_kernel
            .published_engine(&id)
            .unwrap()
            .terminal()
            .unwrap()
            .title()
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        title(&h),
        b"old-live",
        "history failure must not stop live output"
    );
    assert!(!h.send(history_page(&id, b"stale")).resync_required);
    assert!(h.mirror.panes[&id].history_degraded, "the mark persists");

    attach_started(
        &mut h.mirror.engine_kernel,
        &mut h.mirror.kernel_effects,
        10,
        std::slice::from_ref(&id),
    );
    h.send(FrameKind::BootstrapBegin {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: replacement,
        profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
        cols: 80,
        rows: 24,
        base_seq: 0,
    });
    h.send(FrameKind::BootstrapChunk {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: replacement,
        chunk_seq: 0,
        payload: bytes::Bytes::from_static(b"\x1b]2;new\x07"),
    });
    assert_eq!(
        title(&h),
        b"old-live",
        "replacement remains staged until READY"
    );
    let ready = h.send(FrameKind::BootstrapReady {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: replacement,
        history_cursor: None,
    });
    // A fresh replica has fresh history: the mark clears and the badge repaints.
    assert!(!ready.layout_replaced && ready.chrome_dirty);
    assert!(!h.mirror.panes[&id].history_degraded);
    assert_eq!(h.mirror.panes[&id].last_title, "new");
    assert!(
        h.send(FrameKind::AttachReady { attach_id: 10 })
            .layout_replaced
    );
    assert_eq!(h.mirror.panes[&id].last_title, "new");
}

/// Per-pane scrollback loss reaches the status bar, naming the pane.
#[test]
fn history_unavailable_status_names_the_pane_in_a_warn_notice() {
    let id = tid(3);
    let mut h = H::new();
    h.send(begin_frame(&id));
    h.send(ready_frame(&id));
    let outcome = h.send(history_page(&id, b"cursor"));
    assert_eq!(outcome.notices.len(), 1);
    assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
    assert_eq!(outcome.notices[0].text, "pane 3: scrollback unavailable");
}

/// A terminal that is already gone (a popup whose command exited) loses no
/// scrollback anyone can read: `Released` raises no toast.
#[test]
fn a_released_history_raises_no_notice() {
    let id = tid(6);
    let mut h = H::new();
    h.send(begin_frame(&id));
    h.send(FrameKind::BootstrapReady {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        history_cursor: Some(bytes::Bytes::from_static(b"c0")),
    });
    let gone = h.send(FrameKind::HistoryTombstone {
        terminal_id: id,
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        cursor: bytes::Bytes::from_static(b"c0"),
        reason: phux_protocol::wire::frame::HistoryTombstoneReason::Released,
    });
    assert!(gone.notices.is_empty(), "{:?}", gone.notices);
}

/// A server-pruned history boundary marks the pane past the notice's TTL; a
/// loading cache before it leaves the pane unmarked.
#[test]
fn history_tombstone_marks_the_pane_degraded() {
    let id = tid(5);
    let mut h = H::new();
    h.send(begin_frame(&id));
    let ready = h.send(FrameKind::BootstrapReady {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        history_cursor: Some(bytes::Bytes::from_static(b"c0")),
    });
    assert!(
        ready.history_request.is_some(),
        "READY starts a history fetch"
    );
    assert!(
        !h.mirror.panes[&id].history_degraded,
        "a loading cache is healthy"
    );
    assert!(!ready.chrome_dirty);

    let pruned = h.send(FrameKind::HistoryTombstone {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        cursor: bytes::Bytes::from_static(b"c0"),
        reason: phux_protocol::wire::frame::HistoryTombstoneReason::Pruned,
    });
    assert_eq!(pruned.notices.len(), 1, "the transient notice still fires");
    assert!(pruned.chrome_dirty);
    assert!(h.mirror.panes[&id].history_degraded);

    // A repeated tombstone for the advanced cursor is ignored, not a flip.
    let repeat = h.send(FrameKind::HistoryTombstone {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        cursor: bytes::Bytes::from_static(b"c0"),
        reason: phux_protocol::wire::frame::HistoryTombstoneReason::Pruned,
    });
    assert!(!repeat.chrome_dirty);
    assert!(h.mirror.panes[&id].history_degraded);
}
