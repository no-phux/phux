//! Native libghostty implementation of the session-kernel engine boundary.
//!
//! Native bootstrap is the official GHOSTSNP snapshot codec: the server sends
//! the READY prefix, this adapter reconstructs a live terminal, then history
//! units are pulled and authenticated by the engine-owned staged decoder.
//!
//! Replicas never answer terminal queries: the server's canonical terminal is
//! the sole query answerer (`docs/spec/input.md` §6), so no `on_pty_write`
//! callback is installed and libghostty drops the replies it would generate.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    marker::PhantomData,
    rc::Rc,
};

use libghostty_vt::{
    Error as SnapshotError, Terminal as GhosttyTerminal,
    screen::{CellContentTag, CellWide, TrackedGridRef},
    selection::{FormatOptions, Selection},
    snapshot::{CaptureEvent, CaptureOptions, FeedDecoder, FeedIncrementalDecoder},
    style::{Palette, PaletteIndex, RgbColor},
    terminal::{ClipboardWrite, Point, PointCoordinate, PointSpace, ScrollViewport},
};
use phux_protocol::{
    BootstrapCapabilities, BootstrapLimits, BootstrapStreamProfile, EngineCodec, EngineFeatureSet,
};
use thiserror::Error;

use super::{
    BootstrapProgress, CanonicalGeometry, DocumentPoint, DocumentSpace, EngineAdapter,
    EngineDamage, EngineDocumentAdapter, EngineDocumentSelection, EngineEffect, EngineEffectBuffer,
    EngineHistoryProjection, EnginePresentationAdapter, EngineProjectionOrigin,
    EngineProjectionRow, EngineSearchMatch, HistoryApplyOutcome,
};
use crate::history::DocumentAnchorId;

const SYNTH_SCROLLBACK_ROWS: usize = 10_000;
const CONTINUATION_LIMIT: usize = 64 * 1024 * 1024;

mod selection;

mod adapter;
mod document;
mod native;
mod projection;
mod replica;

use document::enforce_history_budget;
use native::{PendingEffects, finish_native, install_effect_hooks, push_history, push_native};
use projection::{
    empty_projection, engine_history_rows, grid_ref_graphemes, history_window, rewrap_history_rows,
    to_ghostty_point, to_ghostty_space, trim_projection_rows,
};
pub use replica::GhosttyReplica;
use replica::{NativeDecoderState, NativeReplica, ReplicaState};

/// Return the client bootstrap capabilities supported by the linked engine.
///
/// Official snapshot v1 is advertised when the linked codec can encode a
/// terminal. Probe failure leaves both synthesized compatibility profiles.
#[must_use]
pub fn native_bootstrap_capabilities(limits: BootstrapLimits) -> BootstrapCapabilities {
    let capabilities = BootstrapCapabilities::new().with_limits(limits);
    if official_snapshot_available() {
        capabilities.with_native(
            EngineCodec::LibghosttySnapshotV1,
            EngineFeatureSet::required_native(),
        )
    } else {
        capabilities
    }
}

fn official_snapshot_available() -> bool {
    let Ok(mut terminal) = GhosttyTerminal::new(2, 2) else {
        return false;
    };
    if terminal
        .set_continuation_max_bytes(CONTINUATION_LIMIT)
        .is_err()
    {
        return false;
    }
    terminal.vt_write(b"ok");
    let Ok(mut capture) = terminal.capture_snapshot(CaptureOptions {
        max_record_bytes: 4096,
        max_pages: 8,
    }) else {
        return false;
    };
    let mut buffer = vec![0; 4096];
    loop {
        match capture.next(&mut buffer) {
            Ok(CaptureEvent::Ready { written }) => return written > 0,
            Ok(CaptureEvent::Record { .. }) => {}
            _ => return false,
        }
    }
}

/// Concrete, current-thread libghostty engine host.
///
/// `limits` are the payload bounds negotiated in `HELLO_OK`; they become hard
/// decoder budgets rather than hints. The `Rc` marker deliberately keeps the
/// host on the same thread as every libghostty object it creates.
///
/// ```compile_fail
/// fn require_thread_safe<T: Send + Sync>() {}
/// require_thread_safe::<phux_client_core::engine::ghostty::GhosttyAdapter>();
/// ```
#[derive(Debug)]
pub struct GhosttyAdapter {
    limits: BootstrapLimits,
    native_available: bool,
    next_anchor_id: u64,
    search_case_sensitive: bool,
    theme: Option<TerminalTheme>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

/// Client-side terminal colours (ADR-0157 decision 4): the default ANSI-16
/// palette plus default foreground, background, and cursor.
///
/// They are installed as libghostty *defaults*, so an application's OSC 4 /
/// 10 / 11 / 12 overrides still win and OSC 104 / 110 / 111 / 112 revert to
/// the theme rather than to libghostty's built-ins. A native bootstrap
/// replaces the replica with the server's state, defaults included, so the
/// adapter re-applies the theme each time a bootstrap finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalTheme {
    /// Palette entries 0..=15.
    pub ansi16: [RgbColor; 16],
    /// Default foreground.
    pub foreground: RgbColor,
    /// Default background.
    pub background: RgbColor,
    /// Default cursor colour.
    pub cursor: RgbColor,
}

/// Install `theme` as `terminal`'s defaults, or clear them back to
/// libghostty's built-ins (the renderer's own fallback) with `None`.
fn apply_terminal_theme(
    terminal: &mut GhosttyTerminal<'_, '_>,
    theme: Option<&TerminalTheme>,
) -> Result<(), GhosttyEngineError> {
    let palette = theme.map(|theme| {
        let mut palette = Palette::default();
        for (index, color) in (0u8..).zip(theme.ansi16) {
            palette.set(PaletteIndex(index), color);
        }
        palette
    });
    terminal.set_default_color_palette(palette)?;
    terminal.set_default_fg_color(theme.map(|theme| theme.foreground))?;
    terminal.set_default_bg_color(theme.map(|theme| theme.background))?;
    terminal.set_default_cursor_color(theme.map(|theme| theme.cursor))?;
    Ok(())
}

impl GhosttyAdapter {
    /// Construct an adapter for one connection's negotiated bootstrap limits.
    #[must_use]
    pub fn new(limits: BootstrapLimits) -> Self {
        Self {
            limits,
            native_available: official_snapshot_available(),
            next_anchor_id: 1,
            search_case_sensitive: true,
            theme: None,
            _not_send_or_sync: PhantomData,
        }
    }

    /// Negotiated payload limits enforced by this adapter.
    #[must_use]
    pub const fn limits(&self) -> BootstrapLimits {
        self.limits
    }

    /// Configure loaded-history matching. Insensitive matching folds ASCII only,
    /// matching Ghostty's native scrollback search without altering text/anchors.
    pub const fn set_search_case_sensitive(&mut self, case_sensitive: bool) {
        self.search_case_sensitive = case_sensitive;
    }

    /// Set the terminal theme every replica receives when its bootstrap
    /// finishes. Live replicas are re-themed by the caller with
    /// [`GhosttyReplica::apply_theme`].
    pub const fn set_theme(&mut self, theme: Option<TerminalTheme>) {
        self.theme = theme;
    }

    /// The terminal theme replicas receive.
    #[must_use]
    pub const fn theme(&self) -> Option<&TerminalTheme> {
        self.theme.as_ref()
    }

    /// Convert scanned history ranges into tracked anchor pairs.
    ///
    /// Any failure releases every anchor already tracked for this search, so a
    /// partially tracked result never leaks tracked grid refs into the replica.
    fn track_search_matches(
        &mut self,
        replica: &mut GhosttyReplica,
        ranges: Vec<(DocumentPoint, DocumentPoint)>,
    ) -> Result<Vec<EngineSearchMatch>, GhosttyEngineError> {
        let mut matches: Vec<EngineSearchMatch> = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            match self.track_match_range(replica, start, end) {
                Ok(found) => matches.push(found),
                Err(error) => {
                    self.release_matches(replica, std::mem::take(&mut matches));
                    return Err(error);
                }
            }
        }
        Ok(matches)
    }

    /// Track both endpoints of one match, releasing the start if the end fails.
    fn track_match_range(
        &mut self,
        replica: &mut GhosttyReplica,
        start: DocumentPoint,
        end: DocumentPoint,
    ) -> Result<EngineSearchMatch, GhosttyEngineError> {
        let start = self.track_document_anchor(replica, start)?;
        let end = match self.track_document_anchor(replica, end) {
            Ok(anchor) => anchor,
            Err(error) => {
                self.release_document_anchor(replica, start);
                return Err(error);
            }
        };
        Ok(EngineSearchMatch { start, end })
    }

    /// Release both anchors of every already-tracked match.
    fn release_matches(&mut self, replica: &mut GhosttyReplica, matches: Vec<EngineSearchMatch>) {
        for found in matches {
            self.release_document_anchor(replica, found.start);
            self.release_document_anchor(replica, found.end);
        }
    }
}

/// Typed failures from the concrete libghostty engine host.
#[derive(Debug, Error)]
pub enum GhosttyEngineError {
    /// The selected stream profile is unavailable from the linked engine.
    #[error("unsupported bootstrap stream profile: {0:?}")]
    UnsupportedProfile(BootstrapStreamProfile),
    /// A normal terminal allocation or query failed.
    #[error("libghostty terminal operation failed: {0}")]
    Terminal(#[from] libghostty_vt::Error),
    /// The incremental checkpoint wrapper rejected the opaque stream.
    #[error("libghostty checkpoint failed after consuming {consumed} bytes: {source}")]
    Checkpoint {
        /// Exact libghostty status; callers never recover it from text.
        source: SnapshotError,
        /// Exact bytes consumed from the submitted fragment.
        consumed: usize,
    },
    /// An envelope for a different exact native codec was received.
    #[error("wrong checkpoint codec version: expected {expected}, got {actual}")]
    WrongCodecVersion {
        /// Negotiated immutable codec version.
        expected: u16,
        /// Version authenticated by the decoder.
        actual: u16,
    },
    /// One borrowed protocol payload exceeded its negotiated frame bound.
    #[error("engine payload is {actual} bytes; negotiated limit is {limit}")]
    PayloadLimitExceeded {
        /// Borrowed payload length.
        actual: usize,
        /// Negotiated maximum payload length.
        limit: usize,
    },
    /// Compatibility profiles do not accept native history pages.
    #[error("history pages are unsupported for bootstrap stream profile: {0:?}")]
    HistoryUnsupported(BootstrapStreamProfile),
    /// A native bootstrap chunk continued past its authenticated READY record.
    #[error("{trailing} trailing bootstrap bytes after READY")]
    TrailingAfterReady {
        /// Unconsumed bytes after authenticated READY.
        trailing: usize,
    },
    /// A bootstrap chunk arrived after authenticated READY.
    #[error("bootstrap input arrived after READY")]
    InputAfterReady,
    /// A history page arrived before protocol READY published the replica.
    #[error("history page arrived before the native replica was published")]
    HistoryBeforePublication,
    /// Live bytes arrived before the native READY transfer completed.
    #[error("native terminal is not ready for live output")]
    LiveOutputBeforeReady,
    /// Bootstrap bytes or a second protocol finish arrived after FINISH.
    #[error("bootstrap input arrived after FINISH")]
    InputAfterFinish,
    /// FINISH was followed by bytes in the same borrowed fragment.
    #[error("{trailing} trailing bootstrap bytes after FINISH")]
    TrailingAfterFinish {
        /// Unconsumed bytes after the authenticated FINISH record.
        trailing: usize,
    },
    /// The wrapper reported transition accounting that cannot make progress.
    #[error("invalid checkpoint transition accounting: consumed {consumed} of {available}")]
    InvalidProgress {
        /// Wrapper-reported byte consumption.
        consumed: usize,
        /// Bytes offered in this transition.
        available: usize,
    },
    /// The native decoder has already failed and cannot be driven again.
    #[error("native checkpoint decoder is no longer usable")]
    DecoderFailed,
}

impl GhosttyEngineError {
    const fn checkpoint(source: SnapshotError, consumed: usize) -> Self {
        Self::Checkpoint { source, consumed }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod theme_tests;
