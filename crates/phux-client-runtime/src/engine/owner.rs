//! Kernel ownership, queries, projection, and publication.

use super::TerminalTheme;
use super::apply::apply_event;
#[cfg(feature = "engine")]
use super::apply::frame_colors;
#[cfg(feature = "engine")]
use super::predict::Predictor;
use super::{
    Adapter, Command, EffectBuffer, EngineApplyError, EngineEvent, EngineOutcome, HashSet,
    InputBlockReason, InputEligibility, KernelDamageKind, KernelEffect, Lifecycle, Query,
    QueuedCommand, ResourceId, SessionKernel, mpsc,
};
#[cfg(feature = "engine")]
use super::{
    Arc, CanonicalGeometry, ClosedReplica, DocumentAnchorId, DocumentPoint, DocumentSpace,
    EngineDocumentPoint, EngineDocumentSelection, EngineError, FrameKey, GhosttyAdapter,
    GhosttyReplica, GridBuffer, GridDamage, GridFrame, GridProjector, HashMap, MouseMode,
    Publication, ReplicaInfo, ReplicaPrompt, Rgb, Scroll, ScrollViewport, Scrollbar, SearchMatch,
    SearchResults, SelectionGestureEvent, SelectionGestureResult, TextRegion, ViewPresentation,
};
#[cfg(feature = "engine")]
use libghostty_vt::selection::gesture::{
    Behavior, Behaviors, DragEvent, Geometry, Gesture, PressEvent,
};
#[cfg(feature = "engine")]
use libghostty_vt::selection::{FormatOptions, Selection};
#[cfg(feature = "engine")]
use libghostty_vt::terminal::{Point, PointCoordinate, PointSpace};

#[cfg(feature = "engine")]
mod document;
#[cfg(feature = "engine")]
mod gesture;
mod query;
mod replica;
mod updates;

#[cfg(feature = "engine")]
use super::BoundedSelectionText;
#[cfg(feature = "engine")]
mod images;
#[cfg(feature = "engine")]
mod render;
#[cfg(feature = "engine")]
mod views;
#[cfg(feature = "engine")]
use views::{Presentation, View};

#[cfg(feature = "engine")]
struct PointerGesture {
    token: u128,
    handle: u64,
    gesture: Gesture<'static>,
}

#[cfg(feature = "engine")]
struct ProjectorSlot {
    token: u128,
    projector: GridProjector,
    images: images::Images,
    /// The presentation the projector last projected (`None` for the
    /// default). Row damage is relative to that projection, so it is exact
    /// only for the same presentation.
    reader: Option<crate::ViewId>,
    /// A failed projection or publication breaks the incremental damage chain.
    valid: bool,
}

pub(super) struct Owner {
    kernel: SessionKernel<Adapter>,
    effects: EffectBuffer,
    /// Terminals with a published, non-removed projection.
    visible: HashSet<ResourceId>,
    #[cfg(feature = "engine")]
    publication: Arc<Publication>,
    #[cfg(feature = "engine")]
    projectors: HashMap<ResourceId, ProjectorSlot>,
    #[cfg(feature = "engine")]
    cell_pixels: HashMap<ResourceId, (u16, u16)>,
    #[cfg(feature = "engine")]
    closed: HashMap<ResourceId, ClosedReplica<GhosttyAdapter>>,
    #[cfg(feature = "engine")]
    pending_releases: HashSet<ResourceId>,
    #[cfg(feature = "engine")]
    anchors: HashMap<u64, (ResourceId, Option<crate::ViewId>, DocumentAnchorId)>,
    #[cfg(feature = "engine")]
    presentations: HashMap<ResourceId, Presentation>,
    #[cfg(feature = "engine")]
    views: HashMap<crate::ViewId, View>,
    #[cfg(feature = "engine")]
    active_view: Option<crate::ViewId>,
    #[cfg(feature = "engine")]
    document_revisions: HashMap<ResourceId, u64>,
    #[cfg(feature = "engine")]
    next_document_revision: u64,
    /// Whether predictive echo is drawn underlined.
    #[cfg(feature = "engine")]
    prediction_underline: bool,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.retire_publications();
    }
}

impl Owner {
    pub(super) fn new(
        kernel: SessionKernel<Adapter>,
        #[cfg(feature = "engine")] publication: Arc<Publication>,
    ) -> Self {
        Self {
            kernel,
            effects: EffectBuffer::new(),
            visible: HashSet::new(),
            #[cfg(feature = "engine")]
            publication,
            #[cfg(feature = "engine")]
            projectors: HashMap::new(),
            #[cfg(feature = "engine")]
            cell_pixels: HashMap::new(),
            #[cfg(feature = "engine")]
            closed: HashMap::new(),
            #[cfg(feature = "engine")]
            pending_releases: HashSet::new(),
            #[cfg(feature = "engine")]
            anchors: HashMap::new(),
            #[cfg(feature = "engine")]
            presentations: HashMap::new(),
            #[cfg(feature = "engine")]
            views: HashMap::new(),
            #[cfg(feature = "engine")]
            active_view: None,
            #[cfg(feature = "engine")]
            document_revisions: HashMap::new(),
            #[cfg(feature = "engine")]
            next_document_revision: 1,
            #[cfg(feature = "engine")]
            prediction_underline: true,
        }
    }

    pub(super) fn run(mut self, commands: &mpsc::Receiver<QueuedCommand>) {
        while let Ok(queued) = commands.recv() {
            crate::perf::OWNER_QUEUE_WAIT.record_elapsed(queued.queued_at);
            let _timed = crate::perf::OWNER_EXECUTE.timer();
            match queued.command {
                Command::Stop(reply) => {
                    self.retire_publications();
                    let _ = reply.send(());
                    break;
                }
                Command::ApplyBatch(events, reply) => {
                    let _ = reply.send(self.apply_batch(events));
                }
                Command::SetTheme(theme, reply) => {
                    self.set_theme(theme);
                    let _ = reply.send(());
                }
                Command::Lifecycle(lifecycle) => self.lifecycle(lifecycle),
                Command::Query(query) => self.query(query),
                #[cfg(feature = "engine")]
                Command::View(command) => self.view_command(command),
                #[cfg(feature = "engine")]
                Command::CatchUp(key, reply) => {
                    let _ = reply.send(self.catch_up(&key));
                }
            }
        }
    }

    fn retire_publications(&mut self) {
        self.visible.clear();
        #[cfg(feature = "engine")]
        {
            for view in self.views.keys() {
                self.publication.remove_view(*view);
            }
            self.views.clear();
            for id in self.presentations.keys() {
                self.publication.remove(id);
            }
            self.presentations.clear();
        }
    }

    fn lifecycle(&mut self, lifecycle: Lifecycle) {
        match lifecycle {
            Lifecycle::Detach(id, reply) => {
                let detached = self.kernel.detach_terminal(&id);
                if detached {
                    self.forget_projection(&id);
                }
                let _ = reply.send(detached);
            }
            Lifecycle::Reset(reply) => {
                self.kernel.release_active_attach();
                #[cfg(not(feature = "engine"))]
                let visible: Vec<_> = self.visible.iter().cloned().collect();
                #[cfg(feature = "engine")]
                let mut visible: Vec<_> = self.visible.iter().cloned().collect();
                #[cfg(feature = "engine")]
                visible.extend(
                    self.closed
                        .keys()
                        .filter(|id| !self.visible.contains(*id))
                        .cloned(),
                );
                for id in visible {
                    let _ = self.kernel.release_terminal(&id);
                    self.forget_projection(&id);
                }
                // Agent streams and released closes publish no projection,
                // so `visible` never names them.
                self.kernel.release_incarnation();
                let _ = reply.send(());
            }
            #[cfg(feature = "engine")]
            Lifecycle::Retain(id, retain) => {
                self.kernel.set_retain_replica_on_close(&id, retain);
            }
            #[cfg(feature = "engine")]
            Lifecycle::Release(id) => {
                self.kernel.set_retain_replica_on_close(&id, false);
                if self.closed.contains_key(&id) {
                    self.pending_releases.insert(id.clone());
                    self.release_closed(&id);
                    self.projectors.remove(&id);
                    self.publication.remove(&id);
                    self.forget_views(&id);
                    self.invalidate_handles(&id);
                    self.presentations.remove(&id);
                    self.document_revisions.remove(&id);
                }
            }
        }
    }

    fn forget_projection(&mut self, id: &ResourceId) {
        self.visible.remove(id);
        #[cfg(feature = "engine")]
        {
            self.closed.remove(id);
            self.pending_releases.remove(id);
            self.projectors.remove(id);
            self.cell_pixels.remove(id);
            self.publication.remove(id);
            self.invalidate_handles(id);
            self.forget_views(id);
            self.presentations.remove(id);
            self.document_revisions.remove(id);
        }
    }

    #[cfg(feature = "engine")]
    fn terminal(
        &self,
        id: &ResourceId,
    ) -> Result<&libghostty_vt::Terminal<'static, 'static>, EngineError> {
        self.replica(id)
            .and_then(GhosttyReplica::terminal)
            .ok_or_else(|| engine_error("terminal has no renderable engine"))
    }

    #[cfg(feature = "engine")]
    fn reset_gesture(&mut self, id: &ResourceId) {
        let Some(mut state) = self
            .presentations
            .get_mut(id)
            .and_then(|state| state.gesture.take())
        else {
            return;
        };
        let current = self.replica_identity(id).map(|identity| identity.0);
        if current == Some(state.token)
            && let Ok(terminal) = self.terminal(id)
        {
            state.gesture.reset(terminal);
        }
    }

    #[cfg(feature = "engine")]
    fn invalidate_handles(&mut self, id: &ResourceId) {
        self.reset_gesture(id);
        self.anchors
            .retain(|_, (owner, view, _)| owner != id || *view != self.active_view);
        if let Some(state) = self.presentations.get_mut(id) {
            state.selection = None;
            state.viewport = None;
            state.search.clear();
            state.gesture_anchors.clear();
        }
    }

    #[cfg(feature = "engine")]
    fn bump_document_revision(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        let revision = self.next_document_revision;
        self.next_document_revision = revision
            .checked_add(1)
            .ok_or_else(|| engine_error("document revision space exhausted"))?;
        self.document_revisions.insert(id.clone(), revision);
        Ok(())
    }
}

#[cfg(feature = "engine")]
fn viewport_scroll(scroll: Scroll) -> Result<ScrollViewport, EngineError> {
    match scroll {
        Scroll::Top => Ok(ScrollViewport::Top),
        Scroll::Bottom => Ok(ScrollViewport::Bottom),
        Scroll::Delta(delta) => isize::try_from(delta)
            .map(ScrollViewport::Delta)
            .map_err(|_| engine_error("scroll delta exceeds isize")),
        Scroll::Row(row) => usize::try_from(row)
            .map(ScrollViewport::Row)
            .map_err(|_| engine_error("scroll row exceeds usize")),
        Scroll::Prompt(_) => Err(engine_error("prompt jumps resolve against a terminal")),
    }
}

#[cfg(feature = "engine")]
fn terminal_defaults(
    terminal: &libghostty_vt::Terminal<'_, '_>,
) -> Result<(Option<Rgb>, Option<Rgb>, bool), EngineError> {
    use libghostty_vt::terminal::Mode;
    let map = |color: libghostty_vt::style::RgbColor| Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    };
    let mut foreground = terminal
        .fg_color()
        .map_err(|error| engine_error(error.to_string()))?
        .map(map);
    let mut background = terminal
        .bg_color()
        .map_err(|error| engine_error(error.to_string()))?
        .map(map);
    let reversed = terminal
        .mode(Mode::REVERSE_COLORS)
        .map_err(|error| engine_error(error.to_string()))?;
    if reversed {
        std::mem::swap(&mut foreground, &mut background);
    }
    Ok((foreground, background, reversed))
}

#[cfg(feature = "engine")]
const fn apply_default_colors(
    colors: &mut super::FrameColors,
    defaults: (Option<Rgb>, Option<Rgb>, bool),
) {
    colors.has_foreground = defaults.0.is_some();
    colors.has_background = defaults.1.is_some();
    colors.reversed = defaults.2;
    if let Some(foreground) = defaults.0 {
        colors.foreground = foreground;
    }
    if let Some(background) = defaults.1 {
        colors.background = background;
    }
}

#[cfg(feature = "engine")]
fn engine_error(message: impl Into<String>) -> EngineError {
    EngineError::Engine(message.into())
}

#[cfg(feature = "engine")]
fn anchor_unavailable(message: &str) -> EngineError {
    EngineError::AnchorUnavailable(message.to_owned())
}

#[cfg(feature = "engine")]
fn document_space(space: u32) -> Result<DocumentSpace, EngineError> {
    match space {
        0 => Ok(DocumentSpace::History),
        1 => Ok(DocumentSpace::Viewport),
        2 => Ok(DocumentSpace::Active),
        _ => Err(engine_error("unknown document point space")),
    }
}

#[cfg(feature = "engine")]
fn apply_terminal_selection(
    terminal: &libghostty_vt::Terminal<'_, '_>,
    start: Option<DocumentPoint>,
    end: Option<DocumentPoint>,
    rectangle: bool,
) -> Result<(), EngineError> {
    let (Some(start), Some(end)) = (start, end) else {
        return terminal
            .set_selection(None)
            .map(|_| ())
            .map_err(|error| engine_error(error.to_string()));
    };
    let start_ref = terminal
        .grid_ref(Point::History(PointCoordinate {
            x: start.x,
            y: start.y,
        }))
        .map_err(|error| engine_error(error.to_string()))?;
    let end_ref = terminal
        .grid_ref(Point::History(PointCoordinate { x: end.x, y: end.y }))
        .map_err(|error| engine_error(error.to_string()))?;
    terminal
        .set_selection(Some(&Selection::new(start_ref, end_ref, rectangle)))
        .map(|_| ())
        .map_err(|error| engine_error(error.to_string()))
}

#[cfg(feature = "engine")]
fn apply_gesture_event<'terminal>(
    terminal: &'terminal libghostty_vt::Terminal<'_, '_>,
    grid_ref: libghostty_vt::screen::GridRef<'terminal>,
    event: SelectionGestureEvent,
    gesture: &mut Gesture<'_>,
) -> Result<Option<Selection<'terminal>>, EngineError> {
    if event.phase == 0 {
        apply_gesture_press(terminal, grid_ref, event, gesture)
    } else {
        apply_gesture_drag(terminal, grid_ref, event, gesture)
    }
}

#[cfg(feature = "engine")]
fn apply_gesture_press<'terminal>(
    terminal: &'terminal libghostty_vt::Terminal<'_, '_>,
    grid_ref: libghostty_vt::screen::GridRef<'terminal>,
    event: SelectionGestureEvent,
    gesture: &mut Gesture<'_>,
) -> Result<Option<Selection<'terminal>>, EngineError> {
    let behavior = match event.clicks {
        2 => Behavior::Word,
        3 => Behavior::Line,
        _ => Behavior::Cell,
    };
    let mut press = PressEvent::new().map_err(|error| engine_error(error.to_string()))?;
    press
        .set_behaviors(&Behaviors::new().with_single_click_behavior(behavior))
        .map_err(|error| engine_error(error.to_string()))?;
    press
        .set_position(event.x, event.y)
        .map_err(|error| engine_error(error.to_string()))?;
    press
        .apply(gesture, terminal, grid_ref)
        .map_err(|error| engine_error(error.to_string()))
}

#[cfg(feature = "engine")]
fn apply_gesture_drag<'terminal>(
    terminal: &'terminal libghostty_vt::Terminal<'_, '_>,
    grid_ref: libghostty_vt::screen::GridRef<'terminal>,
    event: SelectionGestureEvent,
    gesture: &mut Gesture<'_>,
) -> Result<Option<Selection<'terminal>>, EngineError> {
    let mut drag = DragEvent::new().map_err(|error| engine_error(error.to_string()))?;
    drag.set_position(event.x, event.y)
        .map_err(|error| engine_error(error.to_string()))?;
    drag.set_rectangle(event.rectangle)
        .map_err(|error| engine_error(error.to_string()))?;
    drag.apply(
        gesture,
        terminal,
        grid_ref,
        Geometry {
            columns: event.columns,
            cell_width: event.cell_width,
            padding_left: event.padding_left,
            screen_height: event.screen_height,
        },
    )
    .map_err(|error| engine_error(error.to_string()))
}

#[cfg(feature = "engine")]
fn gesture_document_points(
    terminal: &libghostty_vt::Terminal<'_, '_>,
    selected: &Selection<'_>,
) -> Result<(EngineDocumentPoint, EngineDocumentPoint), EngineError> {
    let start = terminal
        .point_from_grid_ref(&selected.start(), PointSpace::History)
        .map_err(|error| engine_error(error.to_string()))?
        .ok_or_else(|| engine_error("lost gesture start"))?;
    let end = terminal
        .point_from_grid_ref(&selected.end(), PointSpace::History)
        .map_err(|error| engine_error(error.to_string()))?
        .ok_or_else(|| engine_error("lost gesture end"))?;
    Ok((
        EngineDocumentPoint {
            space: 0,
            column: start.x,
            row: start.y,
        },
        EngineDocumentPoint {
            space: 0,
            column: end.x,
            row: end.y,
        },
    ))
}

#[cfg(feature = "engine")]
fn monotonic_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;

    static EPOCH: OnceLock<Instant> = OnceLock::new();
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(feature = "engine")]
fn validate_gesture(event: SelectionGestureEvent) -> Result<(), EngineError> {
    if event.phase > 2 || !event.x.is_finite() || !event.y.is_finite() {
        return Err(engine_error("invalid gesture event"));
    }
    if event.phase < 2 && (event.columns == 0 || event.cell_width == 0 || event.screen_height == 0)
    {
        return Err(engine_error("invalid gesture geometry"));
    }
    if event.phase == 0 && !(1..=3).contains(&event.clicks) {
        return Err(engine_error("invalid gesture clicks"));
    }
    Ok(())
}
