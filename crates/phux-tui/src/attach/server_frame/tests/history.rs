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
