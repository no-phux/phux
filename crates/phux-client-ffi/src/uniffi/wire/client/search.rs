//! Terminal-history Find over the runtime's owner-thread document anchors.
//!
//! The engine owns the search: this layer bounds what crosses the FFI,
//! lowers the runtime's anchor handles unchanged, and names the one failure a
//! consumer must act on — a stale match — as its own error. Handles belong to
//! one terminal's default view; each search replaces that terminal's
//! previous handles and leaves every other terminal's alone.

use phux_client_runtime::engine::{EngineError, SEARCH_MATCH_LIMIT, SearchResults};

use super::*;

/// The longest query, in UTF-8 bytes, a search accepts.
pub const SEARCH_QUERY_BYTE_LIMIT: usize = 4096;

/// One match as two opaque document-anchor handles. They stay valid until the
/// next search or clear on the same terminal, until the history row they sit
/// on is evicted, or until the terminal's replica is rebuilt.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionSearchMatch {
    pub start: u64,
    pub end: u64,
}

/// A bounded search result, in document order (oldest history first).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ProjectionSearch {
    pub matches: Vec<ProjectionSearchMatch>,
    /// More matches exist than came back: the requested bound, the runtime
    /// cap, or the terminal's anchor budget cut the list short.
    pub truncated: bool,
}

impl From<SearchResults> for ProjectionSearch {
    fn from(found: SearchResults) -> Self {
        Self {
            matches: found
                .matches
                .into_iter()
                .map(|hit| ProjectionSearchMatch {
                    start: hit.start,
                    end: hit.end,
                })
                .collect(),
            truncated: found.truncated,
        }
    }
}

#[derive(uniffi::Error, Debug, thiserror::Error)]
pub enum SearchError {
    #[error("search query is empty")]
    EmptyQuery,
    #[error("search query exceeds {limit} bytes")]
    QueryTooLong { limit: u32 },
    /// Not connected, an unknown terminal id, or no live engine.
    #[error("no projection for this terminal")]
    Unavailable,
    /// The match was superseded by a newer search or a clear, its history
    /// row was evicted, or the replica was rebuilt. Search again.
    #[error("search match is stale: {reason}")]
    StaleMatch { reason: String },
    #[error("engine error: {reason}")]
    Engine { reason: String },
}

impl From<EngineError> for SearchError {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::AnchorUnavailable(reason) => Self::StaleMatch { reason },
            EngineError::Stopped | EngineError::ProjectionUnavailable => Self::Unavailable,
            EngineError::Engine(reason) => Self::Engine { reason },
            EngineError::Spawn(error) => Self::Engine {
                reason: error.to_string(),
            },
        }
    }
}

#[uniffi::export]
pub fn search_query_byte_limit() -> u32 {
    u32::try_from(SEARCH_QUERY_BYTE_LIMIT).unwrap_or(u32::MAX)
}

#[uniffi::export]
pub fn search_match_limit() -> u32 {
    u32::try_from(SEARCH_MATCH_LIMIT).unwrap_or(u32::MAX)
}

#[uniffi::export]
impl RemoteClient {
    /// Search the terminal's loaded history for `query`, returning at most
    /// `max_matches` (capped at [`search_match_limit`]). Replaces this
    /// terminal's previous search handles; the viewport does not move.
    pub fn search_projection(
        &self,
        terminal_id: String,
        query: String,
        case_sensitive: bool,
        max_matches: u32,
    ) -> Result<ProjectionSearch, SearchError> {
        if query.is_empty() {
            return Err(SearchError::EmptyQuery);
        }
        if query.len() > SEARCH_QUERY_BYTE_LIMIT {
            return Err(SearchError::QueryTooLong {
                limit: search_query_byte_limit(),
            });
        }
        let max_matches = usize::try_from(max_matches).unwrap_or(usize::MAX);
        self.with_terminal(&terminal_id, |client, id| {
            client.search(id, query, case_sensitive, max_matches)
        })
        .ok_or(SearchError::Unavailable)?
        .map(ProjectionSearch::from)
        .map_err(SearchError::from)
    }

    /// Scroll the terminal so the row holding `anchor` (a match's `start`)
    /// is at the top of the viewport. The frame is published before this
    /// returns, so the next `render_projection` shows it.
    pub fn reveal_search_match(&self, terminal_id: String, anchor: u64) -> Result<(), SearchError> {
        self.with_terminal(&terminal_id, |client, id| client.pin_viewport(id, anchor))
            .ok_or(SearchError::Unavailable)?
            .map_err(SearchError::from)
    }

    /// Release this terminal's search handles. The viewport stays where the
    /// last reveal put it; `scroll_projection_to_bottom` returns to live.
    pub fn clear_projection_search(&self, terminal_id: String) -> Result<(), SearchError> {
        self.with_terminal(&terminal_id, Client::clear_search)
            .ok_or(SearchError::Unavailable)?
            .map_err(SearchError::from)
    }
}

#[cfg(test)]
mod tests;
