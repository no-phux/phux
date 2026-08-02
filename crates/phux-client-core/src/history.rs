//! Generation-bound opaque history retention, cursor continuity, and viewport policy.
//!
//! This module never interprets history payload bytes. Only an [`EngineAdapter`](crate::engine::EngineAdapter)
//! may import them and produce semantic projection/search/selection results.

use sha2::{Digest, Sha256};
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
    #[cfg(any(feature = "native-engine", test))]
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
    next_cursor: Option<HistoryCursor>,
    declared_rows: usize,
    payload: Arc<[u8]>,
    materialized_rows: usize,
    pin_count: usize,
}

#[derive(Debug, Clone)]
struct ConsumedPage {
    id: HistoryPageId,
    next_cursor: Option<HistoryCursor>,
    declared_rows: usize,
    digest: [u8; 32],
    accounted_bytes: usize,
}

/// Bounded immutable newest-first history for one terminal generation.
#[derive(Debug)]
pub struct HistoryCache {
    config: HistoryCacheConfig,
    pages: HashMap<HistoryPageId, CachedPage>,
    page_order: VecDeque<HistoryPageId>,
    evictable: VecDeque<HistoryPageId>,
    consumed: VecDeque<ConsumedPage>,
    consumed_bytes: usize,
    anchor_pages: HashMap<DocumentAnchorId, HashSet<HistoryPageId>>,
    next_cursor: Option<HistoryCursor>,
    next_page_seq: Option<u64>,
    request_max_bytes: u32,
    request_max_rows: u32,
    state: HistoryLoadState,
    loaded_bytes: usize,
    materialized_rows: usize,
    pinned_bytes: usize,
    pinned_materialized_rows: usize,
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
            pages: HashMap::new(),
            page_order: VecDeque::new(),
            evictable: VecDeque::new(),
            consumed: VecDeque::new(),
            consumed_bytes: 0,
            anchor_pages: HashMap::new(),
            next_cursor: newest_cursor,
            next_page_seq,
            request_max_bytes: 0,
            request_max_rows: 0,
            state,
            loaded_bytes: 0,
            materialized_rows: 0,
            pinned_bytes: 0,
            pinned_materialized_rows: 0,
            viewport: ViewportAnchor::Tail,
            projection_width: projection_width.max(2),
            unread_rows: 0,
        }
    }

    /// Current presentation and loading status.
    #[must_use]
    pub fn status(&self) -> HistoryStatus {
        HistoryStatus {
            state: self.state,
            loaded_pages: self.pages.len(),
            loaded_bytes: self.loaded_bytes.saturating_add(self.consumed_bytes),
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
        self.projection_width = width.max(2);
    }

    /// Mark the exact next cursor as requested and return it once.
    pub(crate) fn begin_fetch(&mut self) -> Option<HistoryCursor> {
        self.begin_fetch_with_limits(self.config.request_max_bytes, self.config.request_max_rows)
    }

    pub(crate) fn begin_fetch_with_limits(
        &mut self,
        max_bytes: u32,
        max_rows: u32,
    ) -> Option<HistoryCursor> {
        if self.state != HistoryLoadState::Idle || max_bytes == 0 || max_rows == 0 {
            return None;
        }
        let cursor = self.next_cursor.clone()?;
        self.state = HistoryLoadState::Loading;
        self.request_max_bytes = max_bytes;
        self.request_max_rows = max_rows;
        Some(cursor)
    }

    /// Cancel the exact in-flight request without advancing its cursor.
    pub(crate) fn cancel_fetch(&mut self, cursor: &HistoryCursor) -> bool {
        if self.state != HistoryLoadState::Loading || self.next_cursor.as_ref() != Some(cursor) {
            return false;
        }
        self.state = HistoryLoadState::Idle;
        self.request_max_bytes = 0;
        self.request_max_rows = 0;
        true
    }

    pub(crate) fn retry_limits(
        &self,
        required_bytes: u32,
        required_rows: u32,
    ) -> Option<(u32, u32)> {
        if required_bytes == 0
            || required_rows == 0
            || (required_bytes <= self.request_max_bytes && required_rows <= self.request_max_rows)
            || required_bytes > u32::try_from(self.config.max_bytes).unwrap_or(u32::MAX)
            || required_rows > MAX_HISTORY_PAGE_ROWS
            || required_rows > u32::try_from(self.config.max_materialized_rows).unwrap_or(u32::MAX)
        {
            return None;
        }
        Some((
            self.request_max_bytes.max(required_bytes),
            self.request_max_rows.max(required_rows),
        ))
    }

    pub(crate) fn should_auto_continue(&self) -> bool {
        self.state == HistoryLoadState::Idle
            && self.next_cursor.is_some()
            && (self.materialized_rows < self.config.prefetch_rows
                || matches!(self.viewport, ViewportAnchor::Pinned(_)))
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
        if let Some(existing) = self.pages.get(&id) {
            if existing.payload.as_ref() == payload
                && existing.next_cursor.as_ref() == next_cursor
                && existing.declared_rows == declared_rows as usize
            {
                return Ok(HistoryPageCheck::Duplicate(id));
            }
            return Err(HistoryCacheError::DuplicateConflict);
        }
        if let Some(consumed) = self.consumed.iter().find(|entry| entry.id == id) {
            if consumed.digest == payload_digest(payload)
                && consumed.next_cursor.as_ref() == next_cursor
                && consumed.declared_rows == declared_rows as usize
            {
                return Ok(HistoryPageCheck::Duplicate(id));
            }
            return Err(HistoryCacheError::DuplicateConflict);
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
        if payload.is_empty() {
            return Err(HistoryCacheError::EmptyPayload);
        }
        if payload.len() > self.config.max_bytes || payload.len() > self.request_max_bytes as usize
        {
            return Err(HistoryCacheError::PageTooLarge {
                required: payload.len(),
                budget: self.config.max_bytes.min(self.request_max_bytes as usize),
            });
        }
        if declared_rows > self.request_max_rows || declared_rows > MAX_HISTORY_PAGE_ROWS {
            return Err(HistoryCacheError::ProjectionTooLarge {
                required: declared_rows as usize,
                budget: self.request_max_rows as usize,
            });
        }
        let required_bytes = self.pinned_bytes.saturating_add(payload.len());
        if required_bytes > self.config.max_bytes {
            return Err(HistoryCacheError::PinnedBudget {
                required: required_bytes,
                budget: self.config.max_bytes,
            });
        }
        let required_rows = self
            .pinned_materialized_rows
            .saturating_add(declared_rows as usize);
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
        self.pages.insert(
            id.clone(),
            CachedPage {
                next_cursor: next_cursor.clone(),
                declared_rows: declared_rows as usize,
                payload: Arc::from(payload),
                materialized_rows,
                pin_count: 0,
            },
        );
        self.page_order.push_back(id.clone());
        self.evictable.push_back(id.clone());
        let digest = payload_digest(payload);
        let accounted_bytes = 32usize
            .saturating_add(id.cursor.as_bytes().len())
            .saturating_add(
                next_cursor
                    .as_ref()
                    .map_or(0, |cursor| cursor.as_bytes().len()),
            )
            .saturating_add(std::mem::size_of::<u64>() + std::mem::size_of::<u32>());
        self.consumed.push_back(ConsumedPage {
            id: id.clone(),
            next_cursor: next_cursor.clone(),
            declared_rows: declared_rows as usize,
            digest,
            accounted_bytes,
        });
        self.consumed_bytes = self.consumed_bytes.saturating_add(accounted_bytes);
        while self.consumed.len() > 64 {
            let removed = self.consumed.pop_front().expect("ledger is nonempty");
            self.consumed_bytes = self.consumed_bytes.saturating_sub(removed.accounted_bytes);
        }
        self.next_page_seq = next_cursor
            .as_ref()
            .map(|next| if next == &cursor { page_seq + 1 } else { 1 });
        self.next_cursor = next_cursor;
        self.request_max_bytes = 0;
        self.request_max_rows = 0;
        self.state = if self.next_cursor.is_some() {
            HistoryLoadState::Idle
        } else {
            HistoryLoadState::Complete
        };
        self.evict_to_budget();
        Ok(id)
    }

    /// Remaining bounded engine-anchor registrations.
    pub(crate) fn remaining_anchor_capacity(&self) -> usize {
        self.config
            .max_materialized_rows
            .saturating_sub(self.anchor_pages.len())
    }

    /// Reject an operation before creating more engine-owned tracked anchors.
    pub(crate) fn ensure_anchor_capacity(
        &self,
        additional: usize,
    ) -> Result<(), HistoryCacheError> {
        if additional > self.remaining_anchor_capacity() {
            return Err(HistoryCacheError::ProjectionTooLarge {
                required: self.anchor_pages.len().saturating_add(additional),
                budget: self.config.max_materialized_rows,
            });
        }
        Ok(())
    }

    /// Associate an engine-owned anchor with every loaded page it protects.
    pub(crate) fn register_anchor_pages(
        &mut self,
        anchor: DocumentAnchorId,
        pages: impl IntoIterator<Item = HistoryPageId>,
    ) -> Result<(), HistoryCacheError> {
        if !self.anchor_pages.contains_key(&anchor) {
            self.ensure_anchor_capacity(1)?;
        }
        let pages: HashSet<_> = pages.into_iter().collect();
        if pages.iter().any(|page| !self.pages.contains_key(page)) {
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

    /// Mark the requested boundary pruned and deterministically drop all pages and pins.
    pub(crate) fn mark_pruned(&mut self) {
        self.invalidate(HistoryLoadState::Pruned);
    }

    /// Permanently retire the generation and deterministically drop all pages and pins.
    pub(crate) fn tombstone(&mut self) {
        self.invalidate(HistoryLoadState::Tombstoned);
    }

    fn pin(&mut self, id: &HistoryPageId) {
        if let Some(page) = self.pages.get_mut(id) {
            if page.pin_count == 0 {
                self.pinned_bytes = self.pinned_bytes.saturating_add(page.payload.len());
                self.pinned_materialized_rows = self
                    .pinned_materialized_rows
                    .saturating_add(page.materialized_rows);
            }
            page.pin_count = page.pin_count.saturating_add(1);
        }
    }

    fn unpin(&mut self, id: &HistoryPageId) {
        if let Some(page) = self.pages.get_mut(id) {
            if page.pin_count == 1 {
                self.pinned_bytes = self.pinned_bytes.saturating_sub(page.payload.len());
                self.pinned_materialized_rows = self
                    .pinned_materialized_rows
                    .saturating_sub(page.materialized_rows);
                self.evictable.push_back(id.clone());
            }
            page.pin_count = page.pin_count.saturating_sub(1);
        }
    }

    fn evict_to_budget(&mut self) {
        while self.loaded_bytes > self.config.max_bytes {
            let Some(id) = self.evictable.pop_front() else {
                break;
            };
            let removable = self.pages.get(&id).is_some_and(|page| page.pin_count == 0);
            if !removable {
                continue;
            }
            let removed = self.pages.remove(&id).expect("evictable page exists");
            self.loaded_bytes = self.loaded_bytes.saturating_sub(removed.payload.len());
            self.materialized_rows = self
                .materialized_rows
                .saturating_sub(removed.materialized_rows);
            for pages in self.anchor_pages.values_mut() {
                pages.remove(&id);
            }
        }
        if self.page_order.len() > self.pages.len().saturating_mul(2).saturating_add(64) {
            self.page_order.retain(|id| self.pages.contains_key(id));
        }
        self.evict_materializations();
    }

    fn evict_materializations(&mut self) {
        while self.materialized_rows > self.config.max_materialized_rows {
            let Some(id) = self.page_order.iter().rev().find(|id| {
                self.pages
                    .get(*id)
                    .is_some_and(|page| page.pin_count == 0 && page.materialized_rows > 0)
            }) else {
                break;
            };
            let page = self.pages.get_mut(id).expect("ordered page exists");
            self.materialized_rows = self
                .materialized_rows
                .saturating_sub(page.materialized_rows);
            page.materialized_rows = 0;
        }
    }

    fn invalidate(&mut self, state: HistoryLoadState) {
        self.pages.clear();
        self.page_order.clear();
        self.evictable.clear();
        self.anchor_pages.clear();
        self.next_cursor = None;
        self.next_page_seq = None;
        self.consumed.clear();
        self.consumed_bytes = 0;
        self.request_max_bytes = 0;
        self.request_max_rows = 0;
        self.loaded_bytes = 0;
        self.materialized_rows = 0;
        self.pinned_bytes = 0;
        self.pinned_materialized_rows = 0;
        self.viewport = ViewportAnchor::Tail;
        self.unread_rows = 0;
        self.state = state;
    }
}

fn payload_digest(payload: &[u8]) -> [u8; 32] {
    Sha256::digest(payload).into()
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
        assert!(cache.pages.contains_key(&first));
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
    fn tracked_anchor_registrations_are_bounded() {
        let mut cache = HistoryCache::new(config(64, 1), None, 80);
        cache
            .register_anchor_pages(DocumentAnchorId::from_raw(1), std::iter::empty())
            .unwrap();
        assert_eq!(
            cache.register_anchor_pages(DocumentAnchorId::from_raw(2), std::iter::empty()),
            Err(HistoryCacheError::ProjectionTooLarge {
                required: 2,
                budget: 1,
            })
        );
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

    #[test]
    fn empty_anchor_registration_survives_page_eviction() {
        let mut cache = HistoryCache::new(config(4, 1), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        cache
            .accept_page(cursor(1), 1, Some(cursor(1)), 0, 0, b"aaaa")
            .unwrap();
        cache
            .register_anchor_pages(DocumentAnchorId::from_raw(1), std::iter::empty())
            .unwrap();
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        cache
            .accept_page(cursor(1), 2, None, 0, 0, b"bbbb")
            .unwrap();
        assert_eq!(
            cache.register_anchor_pages(DocumentAnchorId::from_raw(2), std::iter::empty()),
            Err(HistoryCacheError::ProjectionTooLarge {
                required: 2,
                budget: 1,
            })
        );
    }

    #[test]
    fn consumed_digest_survives_raw_page_eviction() {
        let mut cache = HistoryCache::new(config(4, 4), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        let first = cache
            .accept_page(cursor(1), 1, Some(cursor(1)), 1, 1, b"aaaa")
            .unwrap();
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        cache
            .accept_page(cursor(1), 2, None, 1, 1, b"bbbb")
            .unwrap();
        assert!(!cache.pages.contains_key(&first));
        assert_eq!(
            cache.check_page(&cursor(1), 1, Some(&cursor(1)), 1, b"aaaa"),
            Ok(HistoryPageCheck::Duplicate(first))
        );
        assert_eq!(
            cache.check_page(&cursor(1), 1, Some(&cursor(1)), 1, b"changed"),
            Err(HistoryCacheError::DuplicateConflict)
        );
    }

    #[test]
    fn retry_requires_growth_within_hard_caps() {
        let mut cache = HistoryCache::new(config(128, 8), Some(cursor(1)), 80);
        assert_eq!(cache.begin_fetch(), Some(cursor(1)));
        assert_eq!(cache.retry_limits(64, 8), None);
        assert_eq!(cache.retry_limits(80, 8), Some((80, 8)));
        assert_eq!(cache.retry_limits(129, 8), None);
        assert_eq!(cache.retry_limits(80, 9), None);
    }

    #[test]
    fn sustained_budget_eviction_keeps_stable_order_storage_bounded() {
        let mut cache = HistoryCache::new(config(1, 1), Some(cursor(1)), 80);
        for page_seq in 1..=1_000 {
            assert_eq!(cache.begin_fetch(), Some(cursor(1)));
            cache
                .accept_page(
                    cursor(1),
                    page_seq,
                    Some(cursor(1)),
                    0,
                    0,
                    &[page_seq as u8],
                )
                .unwrap();
        }
        assert_eq!(cache.pages.len(), 1);
        assert!(cache.page_order.len() <= cache.pages.len() * 2 + 64);
        assert_eq!(cache.consumed.len(), 64);
    }

    #[test]
    fn pruned_anchor_falls_back_to_tail_and_releases_registration() {
        let mut cache = HistoryCache::new(config(64, 1), None, 80);
        let anchor = DocumentAnchorId::from_raw(1);
        cache
            .register_anchor_pages(anchor, std::iter::empty())
            .unwrap();
        cache.pin_viewport(anchor).unwrap();
        cache.mark_pruned();
        assert_eq!(cache.viewport_anchor(), ViewportAnchor::Tail);
        assert_eq!(cache.status().state, HistoryLoadState::Pruned);
        assert_eq!(cache.remaining_anchor_capacity(), 1);
    }
}
