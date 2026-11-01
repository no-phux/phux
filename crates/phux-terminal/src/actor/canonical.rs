//! The canonical terminal cell and the native-engine bookkeeping types.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

/// One native checkpoint binding: one client's pump on one stream. Two pumps
/// from one client use different streams; only recapture of the same pair
/// tombstones the prior generation.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct NativeCursorKey {
    pub(super) owner: u64,
    pub(super) stream_id: phux_protocol::ids::StreamId,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
impl NativeCursorKey {
    pub(super) const fn new(owner: u64, stream_id: phux_protocol::ids::StreamId) -> Self {
        Self { owner, stream_id }
    }
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub(super) struct NativeCursorOwner {
    pub(super) cursor: crate::native_state::OpaqueHistoryCursor,
    pub(super) record_index: usize,
    pub(super) touched: tokio::time::Instant,
    pub(super) next_page_seq: u64,
    pub(super) terminal_id: phux_protocol::ids::ResourceId,
    pub(super) stream_id: phux_protocol::ids::StreamId,
    pub(super) bootstrap_id: phux_protocol::ids::BootstrapId,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub(super) struct PendingNativeBootstrap {
    pub(super) capture: Option<crate::native_state::NativeManagedCapture<'static>>,
    pub(super) waiters: Vec<NativeBootstrapRequest>,
    pub(super) records: Vec<Bytes>,
    pub(super) retained_bytes: usize,
    pub(super) capture_bytes: usize,
    pub(super) scratch: Vec<u8>,
    pub(super) max_chunks: usize,
    pub(super) chunk_bytes: usize,
    pub(super) base_seq: u64,
    pub(super) chunk_count: usize,
    pub(super) limits: phux_protocol::caps::BootstrapLimits,
    pub(super) replay: VecDeque<(u64, Bytes)>,
    pub(super) replay_bytes: usize,
    pub(super) started_at: tokio::time::Instant,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub(super) struct PendingNativeHistory {
    pub(super) request: NativeHistoryRequest,
    pub(super) started_at: tokio::time::Instant,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub(super) struct NativePublicationGeneration {
    pub(super) base_seq: u64,
    pub(super) replay: VecDeque<(u64, Bytes)>,
    pub(super) replay_bytes: usize,
    pub(super) waiting: HashSet<NativeCursorKey>,
}

/// Takes host bytes for a native capture. A test injects one that fails.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) type NativeAlloc =
    Box<dyn Fn(usize) -> Result<Vec<u8>, crate::native_state::NativeStateError>>;

/// A byte buffer of `capacity`, refusing rather than aborting when the host
/// is out of memory.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) fn try_native_bytes(
    capacity: usize,
) -> Result<Vec<u8>, crate::native_state::NativeStateError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| crate::native_state::NativeStateError::OutOfMemory)?;
    Ok(bytes)
}

/// Run `alloc`, reporting a panic in it as out of memory.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) fn reserve_native_bytes(
    alloc: &NativeAlloc,
    capacity: usize,
) -> Result<Vec<u8>, crate::native_state::NativeStateError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| alloc(capacity)))
        .unwrap_or(Err(crate::native_state::NativeStateError::OutOfMemory))
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) fn native_step_bytes(
    capture_bytes: usize,
    retained_bytes: usize,
    chunk_bytes: usize,
) -> Result<usize, crate::native_state::NativeStateError> {
    capture_bytes
        .checked_sub(retained_bytes)
        .filter(|bytes| *bytes != 0)
        .map(|remaining| remaining.min(chunk_bytes))
        .ok_or(crate::native_state::NativeStateError::LimitExceeded)
}

#[derive(Debug)]
pub(super) enum CanonicalTerminal {
    Plain(Option<GhosttyTerminal<'static, 'static>>),
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    Native(crate::native_state::NativeTerminalManager),
}

impl CanonicalTerminal {
    /// The canonical terminal, or `None` while it is on loan (to the native
    /// manager or a snapshot capture); callers degrade rather than abort.
    pub(super) const fn try_terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
        match self {
            Self::Plain(terminal) => terminal.as_ref(),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.try_terminal(),
        }
    }

    /// Degrades rather than aborts if the terminal is on loan.
    pub(super) fn vt_write(&mut self, bytes: &[u8]) {
        match self {
            Self::Plain(Some(terminal)) => terminal.vt_write(bytes),
            Self::Plain(None) => {
                tracing::error!(
                    bytes = bytes.len(),
                    "vt_write with the plain canonical terminal taken; dropping the write"
                );
            }
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.vt_write(bytes),
        }
    }

    /// Refuses rather than aborts when the terminal is taken.
    pub(super) fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> libghostty_vt::error::Result<()> {
        match self {
            Self::Plain(Some(terminal)) => {
                terminal.resize(cols, rows, cell_width_px, cell_height_px)
            }
            Self::Plain(None) => {
                tracing::error!("resize with the plain canonical terminal taken; refusing it");
                Err(libghostty_vt::error::Error::InvalidValue)
            }
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.resize(cols, rows, cell_width_px, cell_height_px),
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn native_manager(
        &mut self,
    ) -> Result<
        &mut crate::native_state::NativeTerminalManager,
        crate::native_state::NativeStateError,
    > {
        if let Self::Plain(slot) = self {
            let terminal = slot
                .take()
                .ok_or(crate::native_state::NativeStateError::InvalidState)?;
            match crate::native_state::NativeTerminalManager::new(
                terminal,
                MAX_NATIVE_HISTORY_CLIENTS,
            ) {
                Ok(manager) => *self = Self::Native(manager),
                Err(failure) => {
                    let error = failure.error;
                    *self = Self::Plain(Some(failure.terminal));
                    return Err(error);
                }
            }
        }
        match self {
            Self::Native(manager) => Ok(manager),
            Self::Plain(_) => Err(crate::native_state::NativeStateError::InvalidState),
        }
    }
}

pub(super) enum NativeActorRequest {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    Bootstrap(NativeBootstrapRequest),
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    Publication(NativePublicationRequest),
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    History(NativeHistoryRequest),
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    Release(NativeReleaseRequest),
    #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
    Disabled,
}

pub(super) struct NativeRequestReceivers {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) bootstrap: mpsc::Receiver<NativeBootstrapRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) publication: mpsc::Receiver<NativePublicationRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) history: mpsc::Receiver<NativeHistoryRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) release: mpsc::Receiver<NativeReleaseRequest>,
}

impl NativeRequestReceivers {
    pub(super) async fn recv(&mut self) -> NativeActorRequest {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            tokio::select! {
                Some(request) = self.bootstrap.recv() => NativeActorRequest::Bootstrap(request),
                Some(request) = self.publication.recv() => NativeActorRequest::Publication(request),
                Some(request) = self.history.recv() => NativeActorRequest::History(request),
                Some(request) = self.release.recv() => NativeActorRequest::Release(request),
                else => std::future::pending().await,
            }
        }
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        {
            let _disabled = NativeActorRequest::Disabled;
            std::future::pending::<NativeActorRequest>().await
        }
    }
}

pub(super) enum NativeOrPty {
    Native(NativeActorRequest),
    Pty(Option<PtyEvent>),
}

pub(super) async fn recv_native_or_pty(
    native: &mut NativeRequestReceivers,
    pty: Option<&mut mpsc::Receiver<PtyEvent>>,
    prefer_native: bool,
) -> NativeOrPty {
    if prefer_native {
        tokio::select! {
            biased;
            request = native.recv() => NativeOrPty::Native(request),
            event = recv_or_pending(pty) => NativeOrPty::Pty(event),
        }
    } else {
        tokio::select! {
            biased;
            event = recv_or_pending(pty) => NativeOrPty::Pty(event),
            request = native.recv() => NativeOrPty::Native(request),
        }
    }
}
