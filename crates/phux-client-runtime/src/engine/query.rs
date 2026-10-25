//! Owner-thread query messages.

use super::{
    AgentStreamInfo, EngineError, EngineEvent, EngineOutcome, InputEligibility, ResourceId, Sender,
    TerminalTheme,
};
#[cfg(feature = "engine")]
use super::{
    Arc, BoundedSelectionText, EngineDocumentPoint, FrameKey, GridFrame, MouseMode, ReplicaInfo,
    ReplicaPrompt, Scroll, SearchResults, SelectionGestureEvent, SelectionGestureResult,
    TextRegion, ViewPresentation, views,
};
pub(super) enum Query {
    HasProjection(ResourceId, Sender<bool>),
    IsClosed(ResourceId, Sender<bool>),
    AgentStreamInfo(ResourceId, Sender<Option<AgentStreamInfo>>),
    HasNativeAgentView(ResourceId, Sender<bool>),
    NativeAgentView(
        ResourceId,
        Sender<Option<phux_protocol::native_agent::NativeView>>,
    ),
    InputEligibility(ResourceId, Sender<InputEligibility>),
    InputReady(ResourceId, Sender<bool>),
    #[cfg(feature = "engine")]
    SelectionTextBounded(
        ResourceId,
        usize,
        Sender<Result<BoundedSelectionText, EngineError>>,
    ),
    #[cfg(feature = "engine")]
    Scroll(
        ResourceId,
        Scroll,
        Sender<Result<EngineOutcome, EngineError>>,
    ),
    #[cfg(feature = "engine")]
    IsAltScreen(ResourceId, Sender<bool>),
    #[cfg(feature = "engine")]
    PredictText(ResourceId, String, Sender<Result<bool, EngineError>>),
    #[cfg(feature = "engine")]
    ClearPredictions(ResourceId, Sender<Result<bool, EngineError>>),
    #[cfg(feature = "engine")]
    Republish(ResourceId, Sender<Result<bool, EngineError>>),
    #[cfg(feature = "engine")]
    ReplicaInfo(ResourceId, Sender<Result<ReplicaInfo, EngineError>>),
    #[cfg(feature = "engine")]
    Present(ResourceId, Sender<Result<ViewPresentation, EngineError>>),
    #[cfg(feature = "engine")]
    MouseMode(ResourceId, Sender<Result<MouseMode, EngineError>>),
    #[cfg(feature = "engine")]
    BracketedPaste(ResourceId, Sender<Result<bool, EngineError>>),
    #[cfg(feature = "engine")]
    Prompt(ResourceId, Sender<ReplicaPrompt>),
    #[cfg(feature = "engine")]
    TrackAnchor(
        ResourceId,
        EngineDocumentPoint,
        Sender<Result<u64, EngineError>>,
    ),
    #[cfg(feature = "engine")]
    ReleaseAnchor(ResourceId, u64, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    ClearPresentation(ResourceId, u64, u64, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    PinViewport(ResourceId, u64, Sender<Result<EngineOutcome, EngineError>>),
    #[cfg(feature = "engine")]
    FollowLive(ResourceId, Sender<Result<EngineOutcome, EngineError>>),
    #[cfg(feature = "engine")]
    SetSelection(ResourceId, u64, u64, bool, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    ClearSelection(ResourceId, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    SelectionText(ResourceId, Sender<Result<Vec<u8>, EngineError>>),
    #[cfg(feature = "engine")]
    Search(
        ResourceId,
        String,
        bool,
        usize,
        Sender<Result<SearchResults, EngineError>>,
    ),
    #[cfg(feature = "engine")]
    ClearSearch(ResourceId, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    Gesture(
        ResourceId,
        SelectionGestureEvent,
        Sender<Result<SelectionGestureResult, EngineError>>,
    ),
    #[cfg(feature = "engine")]
    SelectAll(ResourceId, Sender<Result<bool, EngineError>>),
    #[cfg(feature = "engine")]
    RegionText(
        ResourceId,
        TextRegion,
        usize,
        Sender<Result<BoundedSelectionText, EngineError>>,
    ),
    #[cfg(feature = "engine")]
    CellPixels(ResourceId, u16, u16, Sender<Result<(), EngineError>>),
    #[cfg(not(feature = "engine"))]
    TakeOutput(ResourceId, Sender<Vec<u8>>),
}

pub(super) struct QueuedCommand {
    pub(super) command: Command,
    pub(super) queued_at: std::time::Instant,
}

pub(super) enum Command {
    Stop(Sender<()>),
    SetTheme(Option<TerminalTheme>, Sender<()>),
    ApplyBatch(Vec<EngineEvent>, Sender<Vec<EngineOutcome>>),
    Lifecycle(Lifecycle),
    Query(Query),
    #[cfg(feature = "engine")]
    View(views::ViewCommand),
    /// An acquire found a deferred presentation: project it and hand the
    /// frame back.
    #[cfg(feature = "engine")]
    CatchUp(FrameKey, Sender<Option<Arc<GridFrame>>>),
}

impl Command {
    pub(super) fn send(self, sender: &Sender<QueuedCommand>) -> Result<(), EngineError> {
        sender
            .send(QueuedCommand {
                command: self,
                queued_at: std::time::Instant::now(),
            })
            .map_err(|_| {
                crate::perf::REQUEST_ERRORS.incr();
                EngineError::Stopped
            })
    }
}

pub(super) enum Lifecycle {
    Detach(ResourceId, Sender<bool>),
    Reset(Sender<()>),
    #[cfg(feature = "engine")]
    Retain(ResourceId, bool),
    #[cfg(feature = "engine")]
    Release(ResourceId),
}
