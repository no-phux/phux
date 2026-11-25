//! What one presentation of a terminal keeps: the default one and each view.

use super::render::RetiredFrames;
use super::{
    CanonicalGeometry, DocumentAnchorId, EngineDocumentSelection, PointerGesture, Predictor,
    ResourceId,
};

pub(super) struct Presentation {
    pub(super) predictor: Predictor,
    pub(super) retired: RetiredFrames,
    pub(super) viewport: Option<DocumentAnchorId>,
    pub(super) selection: Option<EngineDocumentSelection>,
    pub(super) gesture: Option<PointerGesture>,
    pub(super) search: Vec<u64>,
    pub(super) gesture_anchors: Vec<u64>,
    pub(super) unread_rows: u64,
    pub(super) tail_distance: Option<u64>,
    // Closed replicas cannot allocate kernel document anchors. Their content
    // is immutable, so an offset is sufficient for retained-close scrolling.
    pub(super) closed_offset: Option<u64>,
    /// The screen on which this view's tracked presentation was established.
    pub(super) alternate: Option<bool>,
}

impl Presentation {
    pub(super) fn new(geometry: CanonicalGeometry) -> Self {
        Self {
            predictor: Predictor::new(geometry.cols, geometry.rows),
            retired: RetiredFrames::default(),
            viewport: None,
            selection: None,
            gesture: None,
            search: Vec::new(),
            gesture_anchors: Vec::new(),
            unread_rows: 0,
            tail_distance: None,
            closed_offset: None,
            alternate: None,
        }
    }
}

pub(super) struct View {
    pub(super) terminal: ResourceId,
    pub(super) presentation: Presentation,
}

impl View {
    pub(super) const fn predictor(&self) -> &Predictor {
        &self.presentation.predictor
    }

    pub(super) const fn predictor_mut(&mut self) -> &mut Predictor {
        &mut self.presentation.predictor
    }
}
