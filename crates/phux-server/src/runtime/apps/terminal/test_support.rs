//! The one place host unit tests get a Terminal: real no-PTY actors, a handle
//! with nothing behind it, and a stub whose channels a test answers itself.
//! Tests outside this app name the engine's vocabulary through here, so only
//! `runtime/apps/terminal` depends on `phux-terminal`.

use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::resource::{ResourceCore, ResourceHandle, ResourceKind};

pub(crate) use phux_terminal::actor::{
    ConsumerAttachOutcome, PaneOutput, ResyncAudience, ResyncReason, ScreenReply, ScreenRequest,
    TerminalHandle, TerminalResource,
};
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(crate) use phux_terminal::actor::{NativeBootstrapReply, NativePublicationReply};
pub(crate) use phux_terminal::grid::SnapshotBytes;
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(crate) use phux_terminal::native_state::NativeStateError;
pub(crate) use phux_terminal::{TerminalActorBundle, TerminalInput, vt};

use phux_terminal::actor::{
    ConsumerAttachRequest, ConsumerDetachRequest, EncodedInputRequest, ResizeReceiver,
    ResizeSender, SnapshotRequest, StateSyncBootstrap, TerminalActor,
};
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
use phux_terminal::actor::{NativeBootstrapRequest, NativePublicationRequest};

/// An unstarted no-PTY actor whose grid holds `seed`.
pub(crate) fn seeded_terminal(cols: u16, rows: u16, seed: &[u8]) -> TerminalActorBundle {
    TerminalActor::new_with_seed(cols, rows, seed).expect("seeded terminal actor")
}

/// An unstarted no-PTY actor with an empty grid.
pub(crate) fn blank_terminal(cols: u16, rows: u16) -> TerminalActorBundle {
    TerminalActor::new(cols, rows).expect("blank terminal actor")
}

/// An unstarted default-scrollback actor under `token`.
pub(crate) fn terminal_under(
    cols: u16,
    rows: u16,
    token: CancellationToken,
) -> TerminalActorBundle {
    TerminalActor::build_with_token_and_colors(
        cols,
        rows,
        None,
        phux_config::ScrollbackLimits::default(),
        token,
        None,
    )
    .expect("terminal actor")
}

/// A Terminal handle with no actor and every sender closed.
pub(crate) fn detached_terminal() -> ResourceHandle {
    let (core, channels) =
        ResourceCore::new(ResourceKind::Terminal, None, CancellationToken::new(), 8);
    drop(core);
    ResourceHandle::new(
        None,
        channels.output,
        channels.control,
        TerminalHandle::detached_for_test(80, 24),
    )
}

/// Receivers for a [`stub_terminal`] handle; every channel stays open so
/// sends succeed, and tests read the ones they observe.
pub(crate) struct StubRx {
    pub(crate) snapshot: mpsc::Receiver<SnapshotRequest>,
    pub(crate) resize: ResizeReceiver,
    pub(crate) encoded_input: mpsc::Receiver<EncodedInputRequest>,
    pub(crate) consumer_attach: mpsc::Receiver<ConsumerAttachRequest>,
    pub(crate) consumer_detach: mpsc::Receiver<ConsumerDetachRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(crate) native_bootstrap: mpsc::Receiver<NativeBootstrapRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(crate) native_publication: mpsc::Receiver<NativePublicationRequest>,
    _keep: Vec<Box<dyn std::any::Any>>,
}

/// A Terminal resource handle with no actor behind it.
pub(crate) fn stub_terminal() -> (ResourceHandle, StubRx) {
    use tokio::sync::mpsc::channel;
    let mut keep: Vec<Box<dyn std::any::Any>> = Vec::new();
    let (input, rx) = channel(8);
    keep.push(Box::new(rx));
    let (screen, rx) = channel(8);
    keep.push(Box::new(rx));
    let (pwd, rx) = channel(8);
    keep.push(Box::new(rx));
    let (process, rx) = channel(8);
    keep.push(Box::new(rx));
    let (consumer_ack, rx) = channel(8);
    keep.push(Box::new(rx));
    let (snapshot, snapshot_rx) = channel(8);
    let (resize, resize_rx) = ResizeSender::channel(8);
    let (encoded_input, encoded_rx) = channel(8);
    let (consumer_attach, attach_rx) = channel(8);
    let (consumer_detach, detach_rx) = channel(8);
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    let (native_bootstrap, native_bootstrap_rx) = channel(8);
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    let (native_publication, native_publication_rx) = channel(8);
    let facet = TerminalHandle {
        consumer_attach,
        consumer_detach,
        consumer_ack,
        upgrade: channel(8).0,
        input,
        encoded_input,
        snapshot,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        native_bootstrap,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        native_publication,
        screen,
        pwd,
        process,
        resize,
        ..TerminalHandle::detached_for_test(80, 24)
    };
    let handle = ResourceHandle::new(None, broadcast::channel(8).0, channel(8).0, facet);
    let rx = StubRx {
        snapshot: snapshot_rx,
        resize: resize_rx,
        encoded_input: encoded_rx,
        consumer_attach: attach_rx,
        consumer_detach: detach_rx,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        native_bootstrap: native_bootstrap_rx,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        native_publication: native_publication_rx,
        _keep: keep,
    };
    (handle, rx)
}

/// An 80x24 VT payload with no scrollback.
pub(crate) fn snapshot_bytes(bytes: &[u8]) -> SnapshotBytes {
    SnapshotBytes {
        cols: 80,
        rows: 24,
        bytes: bytes.to_vec(),
        scrollback: Vec::new(),
    }
}

/// A state-sync consumer registration carrying `bytes` as its bootstrap.
pub(crate) fn state_sync_outcome(bytes: &[u8]) -> ConsumerAttachOutcome {
    ConsumerAttachOutcome {
        tick_managed: true,
        state_sync_bootstrap: Some(StateSyncBootstrap {
            snapshot: snapshot_bytes(bytes),
            base_seq: 0,
        }),
    }
}
