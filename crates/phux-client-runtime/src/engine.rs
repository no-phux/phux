//! The engine owner thread (ADR-0133 decision 4).
//!
//! `SessionKernel` and every engine replica live on one dedicated thread;
//! owned [`EngineEvent`]s go in over a channel and owned [`EngineOutcome`]s
//! come back. With the `engine` feature the adapter is libghostty and every
//! damaged terminal is re-projected and published before the outcome is
//! answered; without it a bounded `ByteAdapter` keeps the transport and
//! control-plane lanes buildable with no Zig toolchain.

#[cfg(feature = "engine")]
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

#[cfg(feature = "engine")]
use libghostty_vt::terminal::ScrollViewport;
#[cfg(feature = "engine")]
use phux_client_core::engine::ghostty::{GhosttyAdapter, GhosttyReplica};
use phux_client_core::engine::{CanonicalGeometry, EngineAdapter};
#[cfg(feature = "engine")]
use phux_client_core::engine::{DocumentPoint, DocumentSpace, EngineDocumentSelection};
#[cfg(feature = "engine")]
use phux_client_core::grid::GridProjector;
use phux_client_core::history::HistoryCacheConfig;
#[cfg(feature = "engine")]
use phux_client_core::history::{DocumentAnchorId, HistoryStatus};
pub use phux_client_core::session::AgentStreamInfo;
#[cfg(feature = "engine")]
use phux_client_core::session::ClosedReplica;
use phux_client_core::session::{
    AgentSessionDeclaration, EffectBuffer, HistoryRejectionReason, HistoryUnavailableReason,
    InputBlockReason, InputEligibility, KernelDamageKind, KernelEffect, KernelError, KernelInput,
    KernelStatus, SessionKernel,
};
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, BootstrapStreamProfile};
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{AgentEvent, CloseReason, TombstoneReason};

#[cfg(feature = "engine")]
use crate::publication::{
    CatchUp, FrameColors, FrameKey, GridBuffer, GridDamage, GridFrame, Publication, Rgb, Scrollbar,
};

#[cfg(feature = "engine")]
type Adapter = GhosttyAdapter;
#[cfg(not(feature = "engine"))]
type Adapter = byte_adapter::ByteAdapter;

mod apply;
/// The headless replica: a bounded byte buffer per terminal.
#[cfg(not(feature = "engine"))]
pub mod byte_adapter;
mod owner;
/// Predictive local echo over a projected grid.
#[cfg(feature = "engine")]
pub mod predict;
mod query;
mod types;
#[cfg(feature = "engine")]
mod views;

use query::{Command, Lifecycle, Query, QueuedCommand};
pub use types::*;

use owner::Owner;

/// The thread-safe handle on the owner thread. Cloning it shares the
/// thread; dropping the last clone stops it.
#[derive(Clone, Debug)]
pub struct EngineHandle {
    /// Shared so the publication's catch-up can hold it weakly: a consumer's
    /// acquire must not keep a dropped owner thread alive.
    commands: Arc<Sender<QueuedCommand>>,
}

impl EngineHandle {
    /// Spawn the owner thread with a kernel built from `config`; with the
    /// `engine` feature it publishes every render into `publication`.
    pub(crate) fn start(
        config: &EngineConfig,
        #[cfg(feature = "engine")] publication: Arc<Publication>,
    ) -> Result<Self, EngineError> {
        let (commands, receiver) = mpsc::channel();
        let commands = Arc::new(commands);
        #[cfg(feature = "engine")]
        publication.install_catch_up(catch_up(Arc::downgrade(&commands)));
        let history = config.history();
        let profile = config.profile;
        #[cfg(feature = "engine")]
        let limits = config.limits;
        std::thread::Builder::new()
            .name("phux-engine-owner".to_owned())
            .spawn(move || {
                #[cfg(feature = "engine")]
                let adapter = GhosttyAdapter::new(limits);
                #[cfg(not(feature = "engine"))]
                let adapter = byte_adapter::ByteAdapter;
                let kernel = SessionKernel::with_history_config(adapter, profile, history);
                Owner::new(
                    kernel,
                    #[cfg(feature = "engine")]
                    publication,
                )
                .run(&receiver);
            })
            .map_err(EngineError::Spawn)?;
        Ok(Self { commands })
    }

    /// Apply one event and wait for its outcome.
    pub(crate) fn apply(&self, event: EngineEvent) -> Result<EngineOutcome, EngineError> {
        self.apply_batch(vec![event])?
            .pop()
            .ok_or(EngineError::Stopped)
    }

    /// Apply a pump's queued events in order, then project each damaged
    /// terminal once. Outcomes retain event order; a fatal outcome ends the
    /// applied prefix so later frames remain unapplied.
    pub(crate) fn apply_batch(
        &self,
        events: Vec<EngineEvent>,
    ) -> Result<Vec<EngineOutcome>, EngineError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        self.request(|reply| Command::ApplyBatch(events, reply))
    }

    /// Install (or clear) the terminal theme on every live replica and every
    /// replica bootstrapped from now on, republishing what is visible.
    pub(crate) fn set_terminal_theme(&self, theme: Option<TerminalTheme>) {
        let _ = self.request(|reply| Command::SetTheme(theme, reply));
    }

    /// Forget a terminal and its projection after the owner thread applies
    /// the lifecycle transition. Returns `false` while an aggregate attach
    /// barrier still owns the terminal.
    #[must_use]
    pub fn detach(&self, terminal_id: ResourceId) -> bool {
        self.request(|reply| Command::Lifecycle(Lifecycle::Detach(terminal_id, reply)))
            .unwrap_or(false)
    }

    /// Release every connection-scoped replica and projection.
    pub(crate) fn reset_connection(&self) {
        let _ = self.request(|reply| Command::Lifecycle(Lifecycle::Reset(reply)));
    }

    /// Retire publications synchronously and reject future requests, including
    /// requests through retained clones of a superseded owner handle.
    pub(crate) fn stop(&self) {
        let _ = self.request(Command::Stop);
    }

    /// Whether the terminal has a live (or explicitly retained) replica.
    #[must_use]
    pub fn has_projection(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::HasProjection(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Whether the kernel has permanently closed the terminal.
    #[must_use]
    pub fn is_closed(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::IsClosed(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// The kernel's committed agent generation and record cursor.
    ///
    /// Returns `None` before successful READY or after detach, closure, or
    /// incarnation replacement. Staged or rejected records never advance it.
    #[must_use]
    pub fn agent_stream_info(&self, terminal_id: &ResourceId) -> Option<AgentStreamInfo> {
        self.request(|reply| Command::Query(Query::AgentStreamInfo(terminal_id.clone(), reply)))
            .ok()
            .flatten()
    }

    /// The exact committed SDK controller view, never inferred from a provider.
    /// Closed sources remain readable but cannot expose connected controls.
    #[must_use]
    pub fn native_agent_view(
        &self,
        agent_session_id: &ResourceId,
    ) -> Option<phux_protocol::native_agent::NativeView> {
        self.request(|reply| {
            Command::Query(Query::NativeAgentView(agent_session_id.clone(), reply))
        })
        .ok()
        .flatten()
    }

    /// Whether the exact session retains native ownership, without copying its view.
    /// A retained view need not be connected or ready to accept commands.
    #[must_use]
    pub fn has_native_agent_view(&self, agent_session_id: &ResourceId) -> bool {
        self.request(|reply| {
            Command::Query(Query::HasNativeAgentView(agent_session_id.clone(), reply))
        })
        .unwrap_or(false)
    }

    /// The kernel's current input eligibility for one terminal.
    #[must_use]
    pub fn input_eligibility(&self, terminal_id: &ResourceId) -> InputEligibility {
        self.request(|reply| Command::Query(Query::InputEligibility(terminal_id.clone(), reply)))
            .unwrap_or(InputEligibility::Ineligible(
                InputBlockReason::UnknownTerminal,
            ))
    }

    /// Whether the kernel would accept input for the terminal right now.
    #[must_use]
    pub fn input_ready(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::InputReady(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Keep the final replica of a terminal after it closes, so a consumer
    /// can still scroll and read it until [`Self::release`].
    #[cfg(feature = "engine")]
    pub fn set_retain_on_close(&self, terminal_id: &ResourceId, retain: bool) {
        let _ =
            Command::Lifecycle(Lifecycle::Retain(terminal_id.clone(), retain)).send(&self.commands);
    }

    /// Release a retained closed replica and its projection.
    #[cfg(feature = "engine")]
    pub fn release(&self, terminal_id: &ResourceId) {
        let _ = Command::Lifecycle(Lifecycle::Release(terminal_id.clone())).send(&self.commands);
    }

    /// Scroll the terminal's viewport and publish the result before
    /// returning.
    #[cfg(feature = "engine")]
    pub(crate) fn scroll(
        &self,
        terminal_id: &ResourceId,
        scroll: Scroll,
    ) -> Result<EngineOutcome, EngineError> {
        self.request(|reply| Command::Query(Query::Scroll(terminal_id.clone(), scroll, reply)))?
    }

    /// Whether the terminal's alternate screen is active.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn is_alt_screen(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::IsAltScreen(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Add a local predictive-text commit and publish its presentation.
    #[cfg(feature = "engine")]
    pub fn predict_text(
        &self,
        terminal_id: &ResourceId,
        text: String,
    ) -> Result<bool, EngineError> {
        self.request(|reply| Command::Query(Query::PredictText(terminal_id.clone(), text, reply)))?
    }

    /// Clear local predictive presentation and republish authority.
    #[cfg(feature = "engine")]
    pub fn clear_predictions(&self, terminal_id: &ResourceId) -> Result<bool, EngineError> {
        self.request(|reply| Command::Query(Query::ClearPredictions(terminal_id.clone(), reply)))?
    }

    #[cfg(feature = "engine")]
    pub(crate) fn set_cell_pixels(
        &self,
        id: &ResourceId,
        width: u16,
        height: u16,
    ) -> Result<(), EngineError> {
        self.request(|reply| Command::Query(Query::CellPixels(id.clone(), width, height, reply)))?
    }

    /// Re-project and publish the terminal now, whether or not the kernel
    /// reported damage. Returns whether a frame was published.
    #[cfg(feature = "engine")]
    pub fn republish(&self, terminal_id: &ResourceId) -> Result<bool, EngineError> {
        self.request(|reply| Command::Query(Query::Republish(terminal_id.clone(), reply)))?
    }

    /// Owned facts about the current replica.
    #[cfg(feature = "engine")]
    pub fn replica_info(&self, terminal_id: &ResourceId) -> Result<ReplicaInfo, EngineError> {
        self.request(|reply| Command::Query(Query::ReplicaInfo(terminal_id.clone(), reply)))?
    }

    /// The terminal's full DEC mouse-tracking mode.
    #[cfg(feature = "engine")]
    pub fn mouse_mode(&self, terminal_id: &ResourceId) -> Result<MouseMode, EngineError> {
        self.request(|reply| Command::Query(Query::MouseMode(terminal_id.clone(), reply)))?
    }

    /// Whether the replica has DEC 2004 bracketed paste on: the mode the
    /// server will encode a paste against, as this replica last saw it.
    #[cfg(feature = "engine")]
    pub fn bracketed_paste(&self, terminal_id: &ResourceId) -> Result<bool, EngineError> {
        self.request(|reply| Command::Query(Query::BracketedPaste(terminal_id.clone(), reply)))?
    }

    /// Where the replica's shell is in its prompt cycle, from its OSC 133
    /// marks. [`ReplicaPrompt::Unknown`] when the owner has stopped.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn replica_prompt(&self, terminal_id: &ResourceId) -> ReplicaPrompt {
        self.request(|reply| Command::Query(Query::Prompt(terminal_id.clone(), reply)))
            .unwrap_or(ReplicaPrompt::Unknown)
    }

    /// Track a document coordinate on the owner thread.
    #[cfg(feature = "engine")]
    pub fn track_anchor(
        &self,
        terminal_id: &ResourceId,
        point: EngineDocumentPoint,
    ) -> Result<u64, EngineError> {
        self.request(|reply| Command::Query(Query::TrackAnchor(terminal_id.clone(), point, reply)))?
    }

    /// Release one document anchor.
    #[cfg(feature = "engine")]
    pub fn release_anchor(&self, terminal_id: &ResourceId, anchor: u64) -> Result<(), EngineError> {
        self.request(|reply| {
            Command::Query(Query::ReleaseAnchor(terminal_id.clone(), anchor, reply))
        })?
    }

    /// Clear local presentation for an exact replica generation.
    #[cfg(feature = "engine")]
    pub fn clear_presentation(
        &self,
        terminal_id: &ResourceId,
        stream_id: u64,
        bootstrap_id: u64,
    ) -> Result<(), EngineError> {
        self.request(|reply| {
            Command::Query(Query::ClearPresentation(
                terminal_id.clone(),
                stream_id,
                bootstrap_id,
                reply,
            ))
        })?
    }

    /// Pin the viewport to an existing document anchor.
    #[cfg(feature = "engine")]
    pub(crate) fn pin_viewport(
        &self,
        terminal_id: &ResourceId,
        anchor: u64,
    ) -> Result<EngineOutcome, EngineError> {
        self.request(|reply| {
            Command::Query(Query::PinViewport(terminal_id.clone(), anchor, reply))
        })?
    }

    /// Follow the live history tail.
    #[cfg(feature = "engine")]
    pub(crate) fn follow_live(
        &self,
        terminal_id: &ResourceId,
    ) -> Result<EngineOutcome, EngineError> {
        self.request(|reply| Command::Query(Query::FollowLive(terminal_id.clone(), reply)))?
    }

    /// Set a document selection from two runtime anchor handles.
    #[cfg(feature = "engine")]
    pub fn set_selection(
        &self,
        terminal_id: &ResourceId,
        start: u64,
        end: u64,
        rectangle: bool,
    ) -> Result<(), EngineError> {
        self.request(|reply| {
            Command::Query(Query::SetSelection(
                terminal_id.clone(),
                start,
                end,
                rectangle,
                reply,
            ))
        })?
    }

    /// Clear a document selection.
    #[cfg(feature = "engine")]
    pub fn clear_selection(&self, terminal_id: &ResourceId) -> Result<(), EngineError> {
        self.request(|reply| Command::Query(Query::ClearSelection(terminal_id.clone(), reply)))?
    }

    /// Format the active selection as owned UTF-8 bytes.
    #[cfg(feature = "engine")]
    pub fn selection_text(&self, terminal_id: &ResourceId) -> Result<Vec<u8>, EngineError> {
        self.request(|reply| Command::Query(Query::SelectionText(terminal_id.clone(), reply)))?
    }

    /// Search loaded history and replace the default view's search handles.
    /// Endpoints used by its active selection remain valid until released.
    #[cfg(feature = "engine")]
    pub fn search(
        &self,
        terminal_id: &ResourceId,
        query: String,
        case_sensitive: bool,
    ) -> Result<Vec<SearchMatch>, EngineError> {
        self.search_bounded(terminal_id, query, case_sensitive, SEARCH_MATCH_LIMIT)
            .map(|found| found.matches)
    }

    /// [`Self::search`] returning at most `max_matches` (capped at
    /// [`SEARCH_MATCH_LIMIT`]) and whether a bound hid more.
    #[cfg(feature = "engine")]
    pub fn search_bounded(
        &self,
        terminal_id: &ResourceId,
        query: String,
        case_sensitive: bool,
        max_matches: usize,
    ) -> Result<SearchResults, EngineError> {
        self.request(|reply| {
            Command::Query(Query::Search(
                terminal_id.clone(),
                query,
                case_sensitive,
                max_matches.min(SEARCH_MATCH_LIMIT),
                reply,
            ))
        })?
    }

    /// Release the default view's search handles without moving its
    /// viewport. Endpoints its active selection uses stay valid.
    #[cfg(feature = "engine")]
    pub fn clear_search(&self, terminal_id: &ResourceId) -> Result<(), EngineError> {
        self.request(|reply| Command::Query(Query::ClearSearch(terminal_id.clone(), reply)))?
    }

    /// Apply one provider-owned selection gesture.
    #[cfg(feature = "engine")]
    pub fn selection_gesture(
        &self,
        terminal_id: &ResourceId,
        event: SelectionGestureEvent,
    ) -> Result<SelectionGestureResult, EngineError> {
        self.request(|reply| Command::Query(Query::Gesture(terminal_id.clone(), event, reply)))?
    }

    /// Drain the bytes the headless replica retained since the last take.
    #[cfg(not(feature = "engine"))]
    #[must_use]
    pub fn take_output(&self, terminal_id: &ResourceId) -> Vec<u8> {
        self.request(|reply| Command::Query(Query::TakeOutput(terminal_id.clone(), reply)))
            .unwrap_or_default()
    }

    fn request<T>(&self, command: impl FnOnce(Sender<T>) -> Command) -> Result<T, EngineError> {
        let _timed = crate::perf::REQUEST_WALL.timer();
        let (reply, response) = mpsc::channel();
        command(reply).send(&self.commands)?;
        response.recv().map_err(|_| {
            crate::perf::REQUEST_ERRORS.incr();
            EngineError::Stopped
        })
    }
}

/// The acquire-side catch-up for a deferred presentation: one owner round
/// trip, and `None` once the owner is gone.
#[cfg(feature = "engine")]
fn catch_up(commands: std::sync::Weak<Sender<QueuedCommand>>) -> CatchUp {
    Arc::new(move |key: &FrameKey| {
        let commands = commands.upgrade()?;
        let _timed = crate::perf::REQUEST_WALL.timer();
        let (reply, response) = mpsc::channel();
        Command::CatchUp(key.clone(), reply).send(&commands).ok()?;
        response.recv().unwrap_or_else(|_| {
            crate::perf::REQUEST_ERRORS.incr();
            None
        })
    })
}

#[cfg(test)]
mod tests;
