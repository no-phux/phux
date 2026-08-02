//! Generation-bound opaque history retention, cursor continuity, and viewport policy.
//!
//! This module never interprets history payload bytes. Only an [`EngineAdapter`](crate::engine::EngineAdapter)
//! may import them and produce semantic projection/search/selection results.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// Hard client-side row cap for one progressive history response.
pub const MAX_HISTORY_PAGE_ROWS: u32 = 4096;

/// Client-local history bounds and prefetch policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryCacheConfig {
    /// Maximum retained opaque history bytes.
    pub max_bytes: usize,
    /// Maximum adapter-reported materialized projection rows.
    pub max_materialized_rows: usize,
    /// Trigger a prefetch within this many rows of the oldest loaded row.
    pub prefetch_rows: usize,
    /// Maximum bytes requested from the server per page.
    pub request_max_bytes: u32,
    /// Maximum rows requested from the server per page.
    pub request_max_rows: u32,
}

impl Default for HistoryCacheConfig {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_materialized_rows: 16_384,
            prefetch_rows: 128,
            request_max_bytes: 1024 * 1024,
            request_max_rows: 1024,
        }
    }
}

impl HistoryCacheConfig {
    /// Clamp caller policy to non-zero protocol and cache bounds.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.max_bytes = self.max_bytes.max(1);
        self.max_materialized_rows = self.max_materialized_rows.max(1);
        self.request_max_bytes = self
            .request_max_bytes
            .max(1)
            .min(u32::try_from(self.max_bytes).unwrap_or(u32::MAX));
        self.request_max_rows = self
            .request_max_rows
            .max(1)
            .min(MAX_HISTORY_PAGE_ROWS)
            .min(u32::try_from(self.max_materialized_rows).unwrap_or(u32::MAX));
        self
    }
}

/// Opaque generation-bound server cursor.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct HistoryCursor(Arc<[u8]>);

impl std::fmt::Debug for HistoryCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HistoryCursor")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

impl HistoryCursor {
    /// Copy an opaque cursor supplied by the selected history transport.
    #[must_use]
    pub(crate) fn new(bytes: &[u8]) -> Self {
        Self(Arc::from(bytes))
    }

    /// Borrow the bytes solely for a matching history request.
    #[must_use]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Stable cache identity of one immutable engine history page.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct HistoryPageId {
    cursor: HistoryCursor,
    page_seq: u64,
}

impl std::fmt::Debug for HistoryPageId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HistoryPageId")
            .field("cursor", &self.cursor)
            .field("page_seq", &self.page_seq)
            .finish()
    }
}

/// Cache-local identifier matching an engine-owned tracked document anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentAnchorId(u64);

impl DocumentAnchorId {
    /// Construct an identifier allocated by an engine adapter.
    #[must_use]
    pub(crate) const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    /// Return the identifier for matching an engine adapter anchor registry.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A frontend viewport follows live output or a stable engine document anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportAnchor {
    /// Follow the live tail.
    Tail,
    /// Keep an engine-tracked document location fixed on screen.
    Pinned(DocumentAnchorId),
}

/// Why progressive history is not currently ready for projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryLoadState {
    /// No request is outstanding; more history may be requested.
    Idle,
    /// The current cursor has one request outstanding.
    Loading,
    /// The engine reached authenticated FINISH.
    Complete,
    /// A response did not consume the exact expected cursor.
    Gap,
    /// The generation changed before the request completed.
    Stale,
    /// The server pruned the requested boundary.
    Pruned,
    /// The generation was permanently retired.
    Tombstoned,
}

/// Frontend presentation state for progressive history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryStatus {
    /// Current load state.
    pub state: HistoryLoadState,
    /// Retained immutable pages.
    pub loaded_pages: usize,
    /// Retained opaque bytes.
    pub loaded_bytes: usize,
    /// Adapter-reported materialized projection rows.
    pub materialized_rows: usize,
    /// Live rows received while the viewport was pinned.
    pub unread_rows: u64,
    /// Cursor that must be requested next, if any.
    pub next_cursor: Option<HistoryCursor>,
    /// Next required non-zero page sequence for the current cursor.
    pub next_page_seq: Option<u64>,
}

/// Classification of an incoming immutable page before engine application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryPageCheck {
    /// This exact cursor has not been applied yet.
    New,
    /// This byte-identical response was already applied.
    Duplicate(HistoryPageId),
}

/// Deterministic progressive-history failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HistoryCacheError {
    /// The page does not consume the exact requested cursor.
    #[error("history cursor gap")]
    Gap,
    /// A duplicate cursor carried different immutable content.
    #[error("history duplicate disagrees with cached page")]
    DuplicateConflict,
    /// A non-zero page sequence was not the exact expected successor.
    #[error("history page sequence gap: expected {expected}, got {actual}")]
    SequenceGap {
        /// Exact next sequence required by this cursor chain.
        expected: u64,
        /// Sequence supplied by the response.
        actual: u64,
    },
    /// A non-final page would overflow its cursor-local sequence.
    #[error("history page sequence exhausted")]
    SequenceExhausted,
    /// A history page payload must never be empty.
    #[error("history page payload is empty")]
    EmptyPayload,
    /// One page alone exceeds the configured opaque byte budget.
    #[error("history page requires {required} bytes, budget is {budget}")]
    PageTooLarge {
        /// Required payload bytes.
        required: usize,
        /// Configured maximum bytes.
        budget: usize,
    },
    /// Adapter projection accounting would exceed the configured row budget.
    #[error("history projection requires {required} rows, budget is {budget}")]
    ProjectionTooLarge {
        /// Declared or materialized rows required.
        required: usize,
        /// Configured maximum rows.
        budget: usize,
    },
    /// Pinned opaque pages leave insufficient space for this response.
    #[error("pinned history requires {required} bytes, budget is {budget}")]
    PinnedBudget {
        /// Bytes required while preserving pinned pages.
        required: usize,
        /// Configured maximum bytes.
        budget: usize,
    },
    /// Pinned projections leave insufficient rows for this response.
    #[error("pinned history projection requires {required} rows, budget is {budget}")]
    PinnedProjectionBudget {
        /// Rows required while preserving pinned projections.
        required: usize,
        /// Configured maximum rows.
        budget: usize,
    },
    /// The page was evicted or never loaded.
    #[error("history page is not loaded")]
    PageUnavailable,
    /// The requested engine anchor is invalidated or unknown.
    #[error("document anchor is unavailable")]
    AnchorUnavailable,
}

#[derive(Debug, Clone)]
struct CachedPage {
    id: HistoryPageId,
    next_cursor: Option<HistoryCursor>,
    declared_rows: usize,
    payload: Arc<[u8]>,
    materialized_rows: usize,
    pin_count: usize,
}

/// Bounded immutable newest-first history for one terminal generation.
#[derive(Debug)]
pub struct HistoryCache {
    config: HistoryCacheConfig,
    pages: VecDeque<CachedPage>,
    page_indices: HashMap<HistoryPageId, usize>,
    anchor_pages: HashMap<DocumentAnchorId, HashSet<HistoryPageId>>,
    visible: HashSet<HistoryPageId>,
    selection: HashSet<HistoryPageId>,
    next_cursor: Option<HistoryCursor>,
    next_page_seq: Option<u64>,
    state: HistoryLoadState,
    loaded_bytes: usize,
    materialized_rows: usize,
    viewport: ViewportAnchor,
    projection_width: u16,
    unread_rows: u64,
}

impl HistoryCache {
    /// Start an empty generation at the newest engine cursor.
    #[must_use]
    pub(crate) fn new(
        config: HistoryCacheConfig,
        newest_cursor: Option<HistoryCursor>,
        projection_width: u16,
    ) -> Self {
        let config = config.normalized();
        let state = if newest_cursor.is_some() {
            HistoryLoadState::Idle
        } else {
            HistoryLoadState::Complete
        };
        let next_page_seq = newest_cursor.as_ref().map(|_| 1);
        Self {
            config,
            pages: VecDeque::new(),
            page_indices: HashMap::new(),
            anchor_pages: HashMap::new(),
            visible: HashSet::new(),
            selection: HashSet::new(),
            next_cursor: newest_cursor,
            next_page_seq,
            state,
            loaded_bytes: 0,
            materialized_rows: 0,
            viewport: ViewportAnchor::Tail,
            projection_width: projection_width.max(1),
            unread_rows: 0,
        }
    }

    /// Current presentation and loading status.
    #[must_use]
    pub fn status(&self) -> HistoryStatus {
        HistoryStatus {
            state: self.state,
            loaded_pages: self.pages.len(),
            loaded_bytes: self.loaded_bytes,
            materialized_rows: self.materialized_rows,
            unread_rows: self.unread_rows,
            next_cursor: self.next_cursor.clone(),
            next_page_seq: self.next_page_seq,
        }
    }

    /// Current viewport anchor.
    #[must_use]
    pub const fn viewport_anchor(&self) -> ViewportAnchor {
        self.viewport
    }

    /// Client-local projection width. This never changes canonical PTY geometry.
    #[must_use]
    pub const fn projection_width(&self) -> u16 {
        self.projection_width
    }

    /// Record a frontend-local width for the next cooperative adapter projection.
    pub(crate) fn reproject(&mut self, width: u16) {
        self.projection_width = width.max(1);
    }

    /// Mark the exact next cursor as requested and return it once.
    pub(crate) fn begin_fetch(&mut self) -> Option<HistoryCursor> {
        if self.state != HistoryLoadState::Idle {
            return None;
        }
        let cursor = self.next_cursor.clone()?;
        self.state = HistoryLoadState::Loading;
        Some(cursor)
    }

    /// Cancel the exact in-flight request without advancing its cursor.
    pub(crate) fn cancel_fetch(&mut self, cursor: &HistoryCursor) -> bool {
        if self.state != HistoryLoadState::Loading || self.next_cursor.as_ref() != Some(cursor) {
            return false;
        }
        self.state = HistoryLoadState::Idle;
        true
    }

    pub(crate) fn can_retry(&self, required_bytes: u32, required_rows: u32) -> bool {
        required_bytes > 0
            && required_rows > 0
            && required_bytes <= self.config.request_max_bytes
            && required_rows <= self.config.request_max_rows
    }

    /// Validate ordering and duplicate identity without mutating the cache.
    pub(crate) fn check_page(
        &self,
        cursor: &HistoryCursor,
        page_seq: u64,
        next_cursor: Option<&HistoryCursor>,
        declared_rows: u32,
        payload: &[u8],
    ) -> Result<HistoryPageCheck, HistoryCacheError> {
        let id = HistoryPageId {
            cursor: cursor.clone(),
            page_seq,
        };
        if let Some(index) = self.page_indices.get(&id).copied() {
            let existing = &self.pages[index];
            if existing.payload.as_ref() == payload
                && existing.next_cursor.as_ref() == next_cursor
                && existing.declared_rows == declared_rows as usize
            {
                return Ok(HistoryPageCheck::Duplicate(id));
            }
            return Err(HistoryCacheError::DuplicateConflict);
        }
        if payload.is_empty() {
            return Err(HistoryCacheError::EmptyPayload);
        }
        if payload.len() > self.config.max_bytes
            || payload.len() > self.config.request_max_bytes as usize
        {
            return Err(HistoryCacheError::PageTooLarge {
                required: payload.len(),
                budget: self
                    .config
                    .max_bytes
                    .min(self.config.request_max_bytes as usize),
            });
        }
        if declared_rows > self.config.request_max_rows || declared_rows > MAX_HISTORY_PAGE_ROWS {
            return Err(HistoryCacheError::ProjectionTooLarge {
                required: declared_rows as usize,
                budget: self.config.request_max_rows as usize,
            });
        }
        if self.state != HistoryLoadState::Loading || self.next_cursor.as_ref() != Some(cursor) {
            return Err(HistoryCacheError::Gap);
        }
        let expected = self.next_page_seq.unwrap_or(1);
        if page_seq == 0 || page_seq != expected {
            return Err(HistoryCacheError::SequenceGap {
                expected,
                actual: page_seq,
            });
        }
        if next_cursor == Some(cursor) && page_seq == u64::MAX {
            return Err(HistoryCacheError::SequenceExhausted);
        }
        let pinned_bytes: usize = self
            .pages
            .iter()
            .filter(|page| page.pin_count > 0)
            .map(|page| page.payload.len())
            .sum();
        let required_bytes = pinned_bytes.saturating_add(payload.len());
        if required_bytes > self.config.max_bytes {
            return Err(HistoryCacheError::PinnedBudget {
                required: required_bytes,
                budget: self.config.max_bytes,
            });
        }
        let pinned_rows: usize = self
            .pages
            .iter()
            .filter(|page| page.pin_count > 0)
            .map(|page| page.materialized_rows)
            .sum();
        let required_rows = pinned_rows.saturating_add(declared_rows as usize);
        if required_rows > self.config.max_materialized_rows {
            return Err(HistoryCacheError::PinnedProjectionBudget {
                required: required_rows,
                budget: self.config.max_materialized_rows,
            });
        }
        Ok(HistoryPageCheck::New)
    }

    /// Insert one engine-accepted response in exact cursor-local sequence.
    pub(crate) fn accept_page(
        &mut self,
        cursor: HistoryCursor,
        page_seq: u64,
        next_cursor: Option<HistoryCursor>,
        declared_rows: u32,
        materialized_rows: usize,
        payload: &[u8],
    ) -> Result<HistoryPageId, HistoryCacheError> {
        if let HistoryPageCheck::Duplicate(id) = self.check_page(
            &cursor,
            page_seq,
            next_cursor.as_ref(),
            declared_rows,
            payload,
        )? {
            return Ok(id);
        }
        let id = HistoryPageId {
            cursor: cursor.clone(),
            page_seq,
        };
        let materialized_rows = materialized_rows
            .min(declared_rows as usize)
            .min(self.config.max_materialized_rows);
        self.loaded_bytes += payload.len();
        self.materialized_rows += materialized_rows;
        self.pages.push_back(CachedPage {
            id: id.clone(),
            next_cursor: next_cursor.clone(),
            declared_rows: declared_rows as usize,
            payload: Arc::from(payload),
            materialized_rows,
            pin_count: 0,
        });
        self.page_indices.insert(id.clone(), self.pages.len() - 1);
        self.next_page_seq = next_cursor
            .as_ref()
            .map(|next| if next == &cursor { page_seq + 1 } else { 1 });
        self.next_cursor = next_cursor;
        self.state = if self.next_cursor.is_some() {
            HistoryLoadState::Idle
        } else {
            HistoryLoadState::Complete
        };
        self.evict_to_budget();
        Ok(id)
    }

    /// Borrow immutable opaque bytes for engine import or diagnostics.
    #[must_use]
    pub(crate) fn payload(&self, page: &HistoryPageId) -> Option<&[u8]> {
        let index = self.page_indices.get(page).copied()?;
        Some(&self.pages[index].payload)
    }

    /// Record adapter-owned projection materialization accounting for one page.
    pub(crate) fn set_materialized_rows(
        &mut self,
        page: &HistoryPageId,
        rows: usize,
    ) -> Result<(), HistoryCacheError> {
        let index = self
            .page_indices
            .get(page)
            .copied()
            .ok_or(HistoryCacheError::PageUnavailable)?;
        let old = self.pages[index].materialized_rows;
        self.materialized_rows = self
            .materialized_rows
            .saturating_sub(old)
            .saturating_add(rows);
        self.pages[index].materialized_rows = rows;
        self.evict_materializations();
        if self.materialized_rows > self.config.max_materialized_rows {
            let required = self.materialized_rows;
            self.materialized_rows = self
                .materialized_rows
                .saturating_sub(rows)
                .saturating_add(old);
            self.pages[index].materialized_rows = old;
            return Err(HistoryCacheError::ProjectionTooLarge {
                required,
                budget: self.config.max_materialized_rows,
            });
        }
        Ok(())
    }

    /// Replace the pages protected by the visible adapter projection.
    pub(crate) fn set_visible_pages(
        &mut self,
        pages: impl IntoIterator<Item = HistoryPageId>,
    ) -> Result<(), HistoryCacheError> {
        let next: HashSet<_> = pages.into_iter().collect();
        if next
            .iter()
            .any(|page| !self.page_indices.contains_key(page))
        {
            return Err(HistoryCacheError::PageUnavailable);
        }
        let removed: Vec<_> = self.visible.difference(&next).cloned().collect();
        let added: Vec<_> = next.difference(&self.visible).cloned().collect();
        for page in &removed {
            self.unpin(page);
        }
        for page in &added {
            self.pin(page);
        }
        self.visible = next;
        self.evict_to_budget();
        Ok(())
    }

    /// Associate an engine-owned anchor with every loaded page it protects.
    pub(crate) fn register_anchor_pages(
        &mut self,
        anchor: DocumentAnchorId,
        pages: impl IntoIterator<Item = HistoryPageId>,
    ) -> Result<(), HistoryCacheError> {
        let pages: HashSet<_> = pages.into_iter().collect();
        if pages
            .iter()
            .any(|page| !self.page_indices.contains_key(page))
        {
            return Err(HistoryCacheError::PageUnavailable);
        }
        if let Some(old) = self.anchor_pages.remove(&anchor) {
            for page in old {
                self.unpin(&page);
            }
        }
        for page in &pages {
            self.pin(page);
        }
        self.anchor_pages.insert(anchor, pages);
        Ok(())
    }

    /// Drop one adapter anchor and its cache pins.
    pub(crate) fn remove_anchor(&mut self, anchor: DocumentAnchorId) {
        if let Some(pages) = self.anchor_pages.remove(&anchor) {
            for page in pages {
                self.unpin(&page);
            }
        }
        if self.viewport == ViewportAnchor::Pinned(anchor) {
            self.viewport = ViewportAnchor::Tail;
            self.unread_rows = 0;
        }
        self.evict_to_budget();
    }

    /// Pin the viewport to an existing engine-owned tracked anchor.
    pub(crate) fn pin_viewport(
        &mut self,
        anchor: DocumentAnchorId,
    ) -> Result<(), HistoryCacheError> {
        if !self.anchor_pages.contains_key(&anchor) {
            return Err(HistoryCacheError::AnchorUnavailable);
        }
        self.viewport = ViewportAnchor::Pinned(anchor);
        Ok(())
    }

    /// Resume following the live tail and clear unread output.
    pub(crate) fn follow_tail(&mut self) {
        self.viewport = ViewportAnchor::Tail;
        self.unread_rows = 0;
    }

    /// Account live output without changing a pinned document location.
    pub(crate) fn note_live_output(&mut self, rows: u64) {
        if matches!(self.viewport, ViewportAnchor::Pinned(_)) {
            self.unread_rows = self.unread_rows.saturating_add(rows);
        }
    }
    /// Mark cursor continuity broken and prevent further fetches.
    pub(crate) fn mark_gap(&mut self) {
        self.invalidate(HistoryLoadState::Gap);
    }

    /// Protect every loaded page touched by an engine-owned selection.
    pub(crate) fn set_selection_pages(
        &mut self,
        pages: impl IntoIterator<Item = HistoryPageId>,
    ) -> Result<(), HistoryCacheError> {
        let next: HashSet<_> = pages.into_iter().collect();
        if next
            .iter()
            .any(|page| !self.page_indices.contains_key(page))
        {
            return Err(HistoryCacheError::PageUnavailable);
        }
        let removed: Vec<_> = self.selection.difference(&next).cloned().collect();
        let added: Vec<_> = next.difference(&self.selection).cloned().collect();
        for page in &removed {
            self.unpin(page);
        }
        for page in &added {
            self.pin(page);
        }
        self.selection = next;
        self.evict_to_budget();
        Ok(())
    }

    /// Whether scrolling this close to the oldest loaded row should prefetch.
    #[must_use]
    pub(crate) fn should_prefetch(&self, rows_from_oldest: usize) -> bool {
        self.state == HistoryLoadState::Idle
            && self.next_cursor.is_some()
            && rows_from_oldest <= self.config.prefetch_rows
    }

    pub(crate) fn has_continuation(&self) -> bool {
        self.next_cursor.is_some()
    }

    pub(crate) fn all_page_ids(&self) -> Vec<HistoryPageId> {
        self.pages.iter().map(|page| page.id.clone()).collect()
    }

    pub(crate) fn invalidate_cursor(
        &mut self,
        cursor: &HistoryCursor,
        state: HistoryLoadState,
    ) -> bool {
        if self.next_cursor.as_ref() != Some(cursor) {
            return false;
        }
        self.invalidate(state);
        true
    }

    /// Mark the generation stale and deterministically drop all pages and pins.
    pub(crate) fn mark_stale(&mut self) {
        self.invalidate(HistoryLoadState::Stale);
    }

    /// Mark the requested boundary pruned and deterministically drop all pages and pins.
    pub(crate) fn mark_pruned(&mut self) {
        self.invalidate(HistoryLoadState::Pruned);
    }

    /// Permanently retire the generation and deterministically drop all pages and pins.
    pub(crate) fn tombstone(&mut self) {
        self.invalidate(HistoryLoadState::Tombstoned);
    }

    fn pin(&mut self, page: &HistoryPageId) {
        if let Some(index) = self.page_indices.get(page).copied() {
            self.pages[index].pin_count += 1;
        }
    }

    fn unpin(&mut self, page: &HistoryPageId) {
        if let Some(index) = self.page_indices.get(page).copied() {
            self.pages[index].pin_count = self.pages[index].pin_count.saturating_sub(1);
        }
    }

    fn evict_to_budget(&mut self) {
        while self.loaded_bytes > self.config.max_bytes {
            let Some(index) = self.pages.iter().position(|page| page.pin_count == 0) else {
                break;
            };
            let removed = self.pages.remove(index).expect("existing page");
            self.loaded_bytes = self.loaded_bytes.saturating_sub(removed.payload.len());
            self.materialized_rows = self
                .materialized_rows
                .saturating_sub(removed.materialized_rows);
            self.visible.remove(&removed.id);
            self.selection.remove(&removed.id);
            self.anchor_pages.retain(|_, pages| {
                pages.remove(&removed.id);
                !pages.is_empty()
            });
            self.reindex();
        }
        self.evict_materializations();
    }

    fn evict_materializations(&mut self) {
        while self.materialized_rows > self.config.max_materialized_rows {
            let Some(page) = self
                .pages
                .iter_mut()
                .rev()
                .find(|page| page.pin_count == 0 && page.materialized_rows > 0)
            else {
                break;
            };
            self.materialized_rows = self
                .materialized_rows
                .saturating_sub(page.materialized_rows);
            page.materialized_rows = 0;
        }
    }

    fn reindex(&mut self) {
        self.page_indices.clear();
        self.page_indices.extend(
            self.pages
                .iter()
                .enumerate()
                .map(|(index, page)| (page.id.clone(), index)),
        );
    }

    fn invalidate(&mut self, state: HistoryLoadState) {
        self.pages.clear();
        self.page_indices.clear();
        self.anchor_pages.clear();
        self.visible.clear();
        self.selection.clear();
        self.next_cursor = None;
        self.next_page_seq = None;
        self.loaded_bytes = 0;
        self.materialized_rows = 0;
        self.viewport = ViewportAnchor::Tail;
        self.unread_rows = 0;
        self.state = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(bytes: usize, rows: usize) -> HistoryCacheConfig {
        HistoryCacheConfig {
            max_bytes: bytes,
            max_materialized_rows: rows,
            prefetch_rows: 2,
            request_max_bytes: 64,
            request_max_rows: 64,
        }
    }

    fn cursor(value: u8) -> HistoryCursor {
        HistoryCursor::new(&[value])
    }

    #[test]
    fn duplicate_pages_are_idempotent_and_gaps_are_explicit() {
        let mut cache = HistoryCache::new(config(32, 32), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        let page = cache
            .accept_page(cursor(1), 1, Some(cursor(2)), 0, 0, b"opaque-newest")
            .unwrap();
        assert_eq!(
            cache.accept_page(cursor(1), 1, Some(cursor(2)), 0, 0, b"opaque-newest"),
            Ok(page)
        );
        assert_eq!(cache.status().loaded_pages, 1);
        assert_eq!(
            cache.accept_page(cursor(9), 1, None, 0, 0, b"opaque-gap"),
            Err(HistoryCacheError::Gap)
        );
    }

    #[test]
    fn two_clients_keep_independent_widths_and_tail_policy() {
        let mut first = HistoryCache::new(config(64, 64), Some(cursor(1)), 80);
        let mut second = HistoryCache::new(config(64, 64), Some(cursor(1)), 41);
        assert_eq!(first.begin_fetch(), Some(cursor(1)));
        assert_eq!(second.begin_fetch(), Some(cursor(1)));
        let page = first
            .accept_page(cursor(1), 1, None, 0, 0, b"opaque")
            .unwrap();
        second
            .accept_page(cursor(1), 1, None, 0, 0, b"opaque")
            .unwrap();
        let anchor = DocumentAnchorId::from_raw(7);
        first.register_anchor_pages(anchor, [page]).unwrap();
        first.pin_viewport(anchor).unwrap();
        first.note_live_output(3);
        second.note_live_output(3);
        assert_eq!(first.viewport_anchor(), ViewportAnchor::Pinned(anchor));
        assert_eq!(first.status().unread_rows, 3);
        assert_eq!(second.viewport_anchor(), ViewportAnchor::Tail);
        assert_eq!(second.status().unread_rows, 0);
        assert_eq!(first.projection_width(), 80);
        assert_eq!(second.projection_width(), 41);
    }

    #[test]
    fn budget_evicts_unpinned_oldest_never_visible_or_selected() {
        let mut cache = HistoryCache::new(config(8, 8), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        let newest = cache
            .accept_page(cursor(1), 1, Some(cursor(2)), 0, 0, b"1111")
            .unwrap();
        assert_eq!(cache.begin_fetch(), Some(cursor(2)));
        let oldest = cache
            .accept_page(cursor(2), 1, Some(cursor(3)), 0, 0, b"2222")
            .unwrap();
        cache.set_visible_pages([oldest.clone()]).unwrap();
        cache.set_selection_pages([oldest.clone()]).unwrap();
        assert_eq!(cache.begin_fetch(), Some(cursor(3)));
        cache
            .accept_page(cursor(3), 1, None, 0, 0, b"3333")
            .unwrap();
        assert!(cache.payload(&oldest).is_some());
        assert!(cache.payload(&newest).is_none());
    }

    #[test]
    fn materialization_budget_drops_only_unpinned_adapter_projection() {
        let mut cache = HistoryCache::new(config(64, 2), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        let first = cache
            .accept_page(cursor(1), 1, Some(cursor(2)), 0, 0, b"one")
            .unwrap();
        assert_eq!(cache.begin_fetch(), Some(cursor(2)));
        let second = cache.accept_page(cursor(2), 1, None, 0, 0, b"two").unwrap();
        cache.set_visible_pages([second.clone()]).unwrap();
        cache.set_materialized_rows(&second, 2).unwrap();
        cache.set_materialized_rows(&first, 2).unwrap();
        assert_eq!(cache.status().materialized_rows, 2);
    }

    #[test]
    fn invalidation_and_prefetch_are_explicit() {
        let mut cache = HistoryCache::new(config(64, 64), Some(cursor(1)), 80);
        assert!(cache.should_prefetch(2));
        cache.mark_pruned();
        assert_eq!(cache.status().state, HistoryLoadState::Pruned);
        assert!(!cache.should_prefetch(0));
        cache.tombstone();
        assert_eq!(cache.status().state, HistoryLoadState::Tombstoned);
    }

    #[test]
    fn pinned_projection_cannot_overrun_row_budget() {
        let mut cache = HistoryCache::new(config(64, 2), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        let page = cache
            .accept_page(cursor(1), 1, None, 0, 0, b"opaque")
            .unwrap();
        cache.set_visible_pages([page.clone()]).unwrap();
        assert_eq!(
            cache.set_materialized_rows(&page, 3),
            Err(HistoryCacheError::ProjectionTooLarge {
                required: 3,
                budget: 2,
            })
        );
        assert_eq!(cache.status().materialized_rows, 0);
    }

    #[test]
    fn same_cursor_pages_require_strict_nonzero_sequence() {
        let mut cache = HistoryCache::new(config(128, 8), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        let first = cache
            .accept_page(cursor(1), 1, Some(cursor(1)), 1, 1, b"one")
            .unwrap();
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        assert_eq!(
            cache.check_page(&cursor(1), 3, Some(&cursor(1)), 1, b"two"),
            Err(HistoryCacheError::SequenceGap {
                expected: 2,
                actual: 3,
            })
        );
        assert_eq!(
            cache.check_page(&cursor(1), 0, None, 1, b"two"),
            Err(HistoryCacheError::SequenceGap {
                expected: 2,
                actual: 0,
            })
        );
        cache.accept_page(cursor(1), 2, None, 1, 1, b"two").unwrap();
        assert!(cache.payload(&first).is_some());
        assert_eq!(cache.status().next_page_seq, None);
    }

    #[test]
    fn declared_page_rows_are_bounded_before_import() {
        let mut cache = HistoryCache::new(config(128, 8), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        assert_eq!(
            cache.check_page(&cursor(1), 1, None, 65, b"opaque"),
            Err(HistoryCacheError::ProjectionTooLarge {
                required: 65,
                budget: 8,
            })
        );
        assert_eq!(cache.status().next_cursor, Some(cursor(1)));
        assert_eq!(cache.status().state, HistoryLoadState::Loading);
    }

    #[test]
    fn oversized_page_is_rejected_without_advancing_cursor() {
        let mut cache = HistoryCache::new(config(128, 8), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        assert_eq!(
            cache.check_page(&cursor(1), 1, None, 0, &[0; 65]),
            Err(HistoryCacheError::PageTooLarge {
                required: 65,
                budget: 64,
            })
        );
        let status = cache.status();
        assert_eq!(status.state, HistoryLoadState::Loading);
        assert_eq!(status.loaded_pages, 0);
        assert_eq!(status.next_cursor, Some(cursor(1)));
    }
}
