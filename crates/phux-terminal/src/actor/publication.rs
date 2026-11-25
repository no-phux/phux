//! One private lifetime for consumer caches and native capture/publication cuts.
//! Registration, cancellation and exact withdrawal share this state. The actor
//! lends the canonical terminal only while producing a cut; no engine hop is
//! inserted and all references remain on its libghostty owner thread.

use std::collections::{HashMap, VecDeque};

use phux_protocol::ClientId;
use tokio_util::sync::CancellationToken;

use super::{
    ConsumerAttachError, ConsumerSyncState, NativeAlloc, NativeBootstrapRequest, NativeCursorKey,
    NativeCursorOwner, NativePublicationGeneration, PendingNativeBootstrap, PendingNativeHistory,
    sync,
};
use futures_util::StreamExt;

pub(super) struct PublicationOwner {
    pub(super) consumers: HashMap<ClientId, ConsumerSyncState>,
    cancellations: futures_util::stream::FuturesUnordered<sync::ConsumerCancellation>,
    pub(super) force_tick: bool,
    pub(super) native_cursor_owners: HashMap<NativeCursorKey, NativeCursorOwner>,
    pub(super) reflow_tombstoned: Vec<phux_resource::ResyncTarget>,
    pub(super) pending_native_bootstrap: Option<PendingNativeBootstrap>,
    pub(super) native_alloc: NativeAlloc,
    pub(super) native_bootstrap_backlog: VecDeque<NativeBootstrapRequest>,
    pub(super) pending_native_history: Option<PendingNativeHistory>,
    pub(super) native_history_backlog: VecDeque<PendingNativeHistory>,
    pub(super) native_publications:
        HashMap<crate::native_state::OpaqueHistoryCursor, NativePublicationGeneration>,
}

impl Default for PublicationOwner {
    fn default() -> Self {
        Self {
            consumers: HashMap::new(),
            cancellations: futures_util::stream::FuturesUnordered::new(),
            force_tick: false,
            native_cursor_owners: HashMap::new(),
            reflow_tombstoned: Vec::new(),
            pending_native_bootstrap: None,
            native_alloc: Box::new(super::try_native_bytes),
            native_bootstrap_backlog: VecDeque::new(),
            pending_native_history: None,
            native_history_backlog: VecDeque::new(),
            native_publications: HashMap::new(),
        }
    }
}

impl PublicationOwner {
    /// Installation takes cancellation and cache together. Replacing an entry
    /// cancels the old wait before any subsequent capture can bind that stream.
    pub(super) fn install(
        &mut self,
        client: ClientId,
        state: ConsumerSyncState,
    ) -> Result<Option<ConsumerSyncState>, ConsumerAttachError> {
        if state.cancellation.is_cancelled() {
            return Err(ConsumerAttachError::Cancelled);
        }
        self.cancellations.push(futures_util::future::join(
            state.cancellation.clone().cancelled_owned(),
            std::future::ready(sync::PublicationCancellation::Consumer(
                client,
                state.sync.stream_id,
                state.sync.bootstrap_id,
            )),
        ));
        let prior = self.consumers.insert(client, state);
        if let Some(prior) = &prior {
            prior.cancellation.cancel();
        }
        Ok(prior)
    }

    /// A stale cancellation is only a wakeup, never authority over a replacement.
    pub(super) fn withdraw(
        &mut self,
        client: ClientId,
        generation: Option<(
            phux_protocol::ids::StreamId,
            phux_protocol::ids::BootstrapId,
        )>,
    ) -> Option<ConsumerSyncState> {
        let state = self.consumers.get(&client)?;
        if generation
            .is_some_and(|(stream, bootstrap)| !state.sync.matches_generation(stream, bootstrap))
        {
            return None;
        }
        let state = self.consumers.remove(&client)?;
        state.cancellation.cancel();
        Some(state)
    }

    #[allow(
        clippy::future_not_send,
        reason = "native captures stay on the terminal LocalSet"
    )]
    pub(super) async fn next_cancelled(&mut self) -> Option<((), sync::PublicationCancellation)> {
        self.cancellations.next().await
    }

    pub(super) fn watch_native(
        &self,
        cancellation: CancellationToken,
        key: NativeCursorKey,
        bootstrap: phux_protocol::ids::BootstrapId,
        cursor: crate::native_state::OpaqueHistoryCursor,
    ) {
        self.cancellations.push(futures_util::future::join(
            cancellation.cancelled_owned(),
            std::future::ready(sync::PublicationCancellation::Native(
                key, bootstrap, cursor,
            )),
        ));
    }

    #[cfg(test)]
    pub(super) fn cancellation_waits_empty(&self) -> bool {
        self.cancellations.is_empty()
    }
}

mod consumers;
mod native;
