//! Generation-bound opaque history retention, cursor continuity, and viewport policy.
//!
//! This module never interprets history payload bytes. Only an [`EngineAdapter`](crate::engine::EngineAdapter)
//! may import them and produce semantic projection/search/selection results.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

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
}

impl Default for HistoryCacheConfig {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_materialized_rows: 16_384,
            prefetch_rows: 128,
            request_max_bytes: 1024 * 1024,
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
    pub fn new(bytes: &[u8]) -> Self {
        Self(Arc::from(bytes))
    }

    /// Borrow the bytes solely for a matching history request.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Stable cache identity of one immutable engine history page.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct HistoryPageId(HistoryCursor);

impl std::fmt::Debug for HistoryPageId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("HistoryPageId").field(&self.0).finish()
    }
}

/// Cache-local identifier matching an engine-owned tracked document anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentAnchorId(u64);

impl DocumentAnchorId {
    /// Construct an identifier allocated by an engine adapter.
    #[must_use]
    pub const fn from_raw(value: u64) -> Self {
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
    /// One page alone exceeds the configured opaque byte budget.
    #[error("history page requires {required} bytes, budget is {budget}")]
    PageTooLarge { required: usize, budget: usize },
    /// Adapter projection accounting would exceed the configured row budget.
    #[error("history projection requires {required} rows, budget is {budget}")]
    ProjectionTooLarge { required: usize, budget: usize },
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
    pub fn new(
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
        Self {
            config,
            pages: VecDeque::new(),
            page_indices: HashMap::new(),
            anchor_pages: HashMap::new(),
            visible: HashSet::new(),
            selection: HashSet::new(),
            next_cursor: newest_cursor,
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
    pub fn reproject(&mut self, width: u16) {
        self.projection_width = width.max(1);
    }

    /// Mark the exact next cursor as requested and return it once.
    pub fn begin_fetch(&mut self) -> Option<HistoryCursor> {
        if self.state != HistoryLoadState::Idle {
            return None;
        }
        let cursor = self.next_cursor.clone()?;
        self.state = HistoryLoadState::Loading;
        Some(cursor)
    }

    /// Cancel a request without advancing its cursor.
    pub fn cancel_fetch(&mut self) {
        if self.state == HistoryLoadState::Loading {
            self.state = HistoryLoadState::Idle;
        }
    }

    /// Validate ordering and duplicate identity without mutating the cache.
    pub fn check_page(
        &self,
        cursor: &HistoryCursor,
        next_cursor: Option<&HistoryCursor>,
        payload: &[u8],
    ) -> Result<HistoryPageCheck, HistoryCacheError> {
        let id = HistoryPageId(cursor.clone());
        if let Some(index) = self.page_indices.get(&id).copied() {
            let existing = &self.pages[index];
            if existing.payload.as_ref() == payload && existing.next_cursor.as_ref() == next_cursor {
                return Ok(HistoryPageCheck::Duplicate(id));
            }
            return Err(HistoryCacheError::DuplicateConflict);
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
        if self.next_cursor.as_ref() != Some(cursor) {
            return Err(HistoryCacheError::Gap);
        }
        Ok(HistoryPageCheck::New)
    }

    /// Insert one immutable response in newest-to-oldest order after engine acceptance.
    pub fn accept_page(
        &mut self,
        cursor: HistoryCursor,
        next_cursor: Option<HistoryCursor>,
        payload: &[u8],
    ) -> Result<HistoryPageId, HistoryCacheError> {
        if let HistoryPageCheck::Duplicate(id) =
            self.check_page(&cursor, next_cursor.as_ref(), payload)?
        {
            return Ok(id);
        }
        let id = HistoryPageId(cursor);
        self.loaded_bytes += payload.len();
        self.pages.push_back(CachedPage {
            id: id.clone(),
            next_cursor: next_cursor.clone(),
            payload: Arc::from(payload),
            materialized_rows: 0,
            pin_count: 0,
        });
        self.page_indices.insert(id.clone(), self.pages.len() - 1);
        self.next_cursor = next_cursor;
        self.state = if self.next_cursor.is_some() {
            HistoryLoadState::Idle
        } else {
            HistoryLoadState::Complete
        };
        // Appending the expected older page is O(1); reindexing is reserved for
        // bounded eviction, never the ordinary per-page path.
        self.evict_to_budget();
        Ok(id)
    }

    /// Borrow immutable opaque bytes for engine import or diagnostics.
    #[must_use]
    pub fn payload(&self, page: &HistoryPageId) -> Option<&[u8]> {
        let index = self.page_indices.get(page).copied()?;
        Some(&self.pages[index].payload)
    }

    /// Record adapter-owned projection materialization accounting for one page.
    pub fn set_materialized_rows(
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
    pub fn set_visible_pages(
        &mut self,
        pages: impl IntoIterator<Item = HistoryPageId>,
    ) -> Result<(), HistoryCacheError> {
        let next: HashSet<_> = pages.into_iter().collect();
        if next.iter().any(|page| !self.page_indices.contains_key(page)) {
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
    pub fn register_anchor_pages(
        &mut self,
        anchor: DocumentAnchorId,
        pages: impl IntoIterator<Item = HistoryPageId>,
    ) -> Result<(), HistoryCacheError> {
        let pages: HashSet<_> = pages.into_iter().collect();
        if pages.iter().any(|page| !self.page_indices.contains_key(page)) {
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
    pub fn remove_anchor(&mut self, anchor: DocumentAnchorId) {
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
    pub fn pin_viewport(&mut self, anchor: DocumentAnchorId) -> Result<(), HistoryCacheError> {
        if !self.anchor_pages.contains_key(&anchor) {
            return Err(HistoryCacheError::AnchorUnavailable);
        }
        self.viewport = ViewportAnchor::Pinned(anchor);
        Ok(())
    }

    /// Resume following the live tail and clear unread output.
    pub fn follow_tail(&mut self) {
        self.viewport = ViewportAnchor::Tail;
        self.unread_rows = 0;
    }

    /// Account live output without changing a pinned document location.
    pub fn note_live_output(&mut self, rows: u64) {
        if matches!(self.viewport, ViewportAnchor::Pinned(_)) {
            self.unread_rows = self.unread_rows.saturating_add(rows);
        }
    }
    /// Mark cursor continuity broken and prevent further fetches.
    pub fn mark_gap(&mut self) {
        self.invalidate(HistoryLoadState::Gap);
    }

    /// Protect every loaded page touched by an engine-owned selection.
    pub fn set_selection_pages(
        &mut self,
        pages: impl IntoIterator<Item = HistoryPageId>,
    ) -> Result<(), HistoryCacheError> {
        let next: HashSet<_> = pages.into_iter().collect();
        if next.iter().any(|page| !self.page_indices.contains_key(page)) {
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
    pub fn should_prefetch(&self, rows_from_oldest: usize) -> bool {
        self.state == HistoryLoadState::Idle
            && self.next_cursor.is_some()
            && rows_from_oldest <= self.config.prefetch_rows
    }

    /// Mark the generation stale and deterministically drop all pages and pins.
    pub fn mark_stale(&mut self) {
        self.invalidate(HistoryLoadState::Stale);
    }

    /// Mark the requested boundary pruned and deterministically drop all pages and pins.
    pub fn mark_pruned(&mut self) {
        self.invalidate(HistoryLoadState::Pruned);
    }

    /// Permanently retire the generation and deterministically drop all pages and pins.
    pub fn tombstone(&mut self) {
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
            let Some(index) = self.pages.iter().rposition(|page| page.pin_count == 0) else {
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
            self.materialized_rows = self.materialized_rows.saturating_sub(page.materialized_rows);
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
            .accept_page(cursor(1), Some(cursor(2)), b"opaque-newest")
            .unwrap();
        assert_eq!(
            cache.accept_page(cursor(1), Some(cursor(2)), b"opaque-newest"),
            Ok(page)
        );
        assert_eq!(cache.status().loaded_pages, 1);
        assert_eq!(
            cache.accept_page(cursor(9), None, b"opaque-gap"),
            Err(HistoryCacheError::Gap)
        );
    }

    #[test]
    fn two_clients_keep_independent_widths_and_tail_policy() {
        let mut first = HistoryCache::new(config(64, 64), Some(cursor(1)), 80);
        let mut second = HistoryCache::new(config(64, 64), Some(cursor(1)), 41);
        let page = first.accept_page(cursor(1), None, b"opaque").unwrap();
        second.accept_page(cursor(1), None, b"opaque").unwrap();
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
        let newest = cache.accept_page(cursor(1), Some(cursor(2)), b"1111").unwrap();
        let oldest = cache.accept_page(cursor(2), Some(cursor(3)), b"2222").unwrap();
        cache.set_visible_pages([oldest.clone()]).unwrap();
        cache.set_selection_pages([oldest.clone()]).unwrap();
        cache.accept_page(cursor(3), None, b"3333").unwrap();
        assert!(cache.payload(&oldest).is_some());
        assert!(cache.payload(&newest).is_none());
    }

    #[test]
    fn materialization_budget_drops_only_unpinned_adapter_projection() {
        let mut cache = HistoryCache::new(config(64, 2), Some(cursor(1)), 80);
        let first = cache.accept_page(cursor(1), Some(cursor(2)), b"one").unwrap();
        let second = cache.accept_page(cursor(2), None, b"two").unwrap();
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
        let page = cache.accept_page(cursor(1), None, b"opaque").unwrap();
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
    fn oversized_page_is_rejected_without_advancing_cursor() {
        let mut cache = HistoryCache::new(config(128, 8), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        assert_eq!(
            cache.check_page(&cursor(1), None, &[0; 65]),
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
