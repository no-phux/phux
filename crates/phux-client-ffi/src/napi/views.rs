//! Metadata and document operations for runtime-owned independent views.
//! Cells remain on the native painter path; no binding-side view registry exists.

use ::napi::{Error, Result};
use napi_derive::napi;
use phux_client_runtime::control::{ControlPlane, TerminalResizeOutcome};
use phux_client_runtime::engine::{
    BoundedSelectionText, EngineDocumentPoint, EngineError, EngineHandle, Scroll,
    SelectionGestureEvent,
};
use phux_client_runtime::{Client, ViewId};
use phux_protocol::ResourceId;

use super::{DesktopClient, terminal_id};

#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err consumes the runtime error"
)]
pub(super) fn engine_error(error: EngineError) -> Error {
    Error::from_reason(error.to_string())
}

pub(super) fn engine(client: &Client) -> Result<EngineHandle> {
    client
        .engine()
        .ok_or_else(|| Error::from_reason("EngineUnavailable"))
}

/// Call under the control-owner lock to prevent reconnect from changing the
/// engine between view validation and a terminal-targeted operation.
pub(super) fn view_terminal(
    client: &Client,
    control: &ControlPlane,
    view: &str,
) -> Result<ResourceId> {
    let view = view_id(view)?;
    control
        .engine()
        .ok_or_else(|| Error::from_reason("EngineUnavailable"))?
        .view_replica_info(view)
        .map_err(engine_error)?;
    client
        .acquire_view(view)
        .map(|frame| frame.terminal_id.clone())
        .ok_or_else(|| Error::from_reason("StaleViewOrProjectionUnavailable"))
}

/// Reject noncanonical, zero, and overflowing handles before reaching the engine.
pub(super) fn handle(value: &str) -> Result<u64> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| Error::from_reason("InvalidHandle"))?;
    if parsed == 0 || parsed.to_string() != value {
        return Err(Error::from_reason("InvalidHandle"));
    }
    Ok(parsed)
}

pub(super) fn view_id(value: &str) -> Result<ViewId> {
    ViewId::from_raw(handle(value)?).ok_or_else(|| Error::from_reason("InvalidViewId"))
}

/// Validate the original JS number, then use the destination's checked parser.
/// No NAPI integer coercion or narrowing cast occurs before these checks.
pub(super) fn integer<T: std::str::FromStr>(value: f64) -> Result<T> {
    if !value.is_finite() || value.fract() != 0.0 || value.abs() > 9_007_199_254_740_991.0 {
        return Err(Error::from_reason("InvalidInteger"));
    }
    value
        .to_string()
        .parse()
        .map_err(|_| Error::from_reason("IntegerOutOfRange"))
}

pub(super) fn finite(value: f64) -> Result<f64> {
    if !value.is_finite() {
        return Err(Error::from_reason("InvalidCoordinate"));
    }
    Ok(value)
}

pub(super) fn bounded_text(text: &str, limit: usize) -> Result<()> {
    if text.len() > limit {
        return Err(Error::from_reason("TextLimitExceeded"));
    }
    Ok(())
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopViewInfo {
    pub view_id: String,
    pub terminal_id: String,
    pub generation: String,
    pub stream_id: String,
    pub bootstrap_id: String,
    pub last_seq: String,
    pub cols: u16,
    pub rows: u16,
    pub scroll_total: String,
    pub scroll_offset: String,
    pub scroll_length: String,
    pub at_tail: bool,
}

/// Local request disposition, never a server acknowledgement.
#[napi(string_enum)]
#[derive(Debug, PartialEq, Eq)]
pub enum DesktopResizeOutcome {
    Queued,
    InvalidSize,
    Observer,
    NotReady,
}

impl From<TerminalResizeOutcome> for DesktopResizeOutcome {
    fn from(value: TerminalResizeOutcome) -> Self {
        match value {
            TerminalResizeOutcome::Queued => Self::Queued,
            TerminalResizeOutcome::InvalidSize => Self::InvalidSize,
            TerminalResizeOutcome::Observer => Self::Observer,
            TerminalResizeOutcome::NotReady => Self::NotReady,
        }
    }
}

pub(super) fn resize_view(
    client: &Client,
    view: &str,
    cols: f64,
    rows: f64,
) -> Result<DesktopResizeOutcome> {
    let cols = integer::<u32>(cols)?;
    let rows = integer::<u32>(rows)?;
    client.with_control(|control| {
        let terminal = view_terminal(client, control, view)?;
        Ok(control.resize_terminal(&terminal, cols, rows).into())
    })
}

const SELECTION_BYTE_LIMIT: usize = 1024 * 1024;

fn selection_text(result: BoundedSelectionText) -> Result<String> {
    let bytes = match result {
        BoundedSelectionText::Text(bytes) => bytes,
        BoundedSelectionText::Unavailable => {
            return Err(Error::from_reason("SelectionUnavailable"));
        }
        BoundedSelectionText::ByteLimitExceeded => {
            return Err(Error::from_reason("SelectionByteLimitExceeded"));
        }
        BoundedSelectionText::WorkLimitExceeded => {
            return Err(Error::from_reason("SelectionWorkLimitExceeded"));
        }
        BoundedSelectionText::Unsupported => {
            return Err(Error::from_reason("BoundedSelectionUnsupported"));
        }
    };
    // Strict decoding cannot expand the byte budget; retain the check as a
    // boundary invariant rather than falling back to an unbounded formatter.
    let text = String::from_utf8(bytes).map_err(|_| Error::from_reason("InvalidSelectionUtf8"))?;
    bounded_text(&text, SELECTION_BYTE_LIMIT)?;
    Ok(text)
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopDocumentPoint {
    /// 0 history, 1 viewport, 2 active screen.
    pub space: f64,
    pub column: f64,
    pub row: f64,
}

impl DesktopDocumentPoint {
    fn decode(self) -> Result<EngineDocumentPoint> {
        let space = integer::<u32>(self.space)?;
        if space > 2 {
            return Err(Error::from_reason("InvalidDocumentSpace"));
        }
        Ok(EngineDocumentPoint {
            space,
            column: integer(self.column)?,
            row: integer(self.row)?,
        })
    }
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopSearchMatch {
    pub start: String,
    pub end: String,
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopSelectionGesture {
    /// 0 press, 1 drag, 2 release. Press has no handle; continuation requires it.
    pub phase: f64,
    pub clicks: f64,
    pub handle: Option<String>,
    pub column: f64,
    pub rectangle: bool,
    pub row: f64,
    pub x: f64,
    pub y: f64,
    pub columns: f64,
    pub cell_width: f64,
    pub screen_height: f64,
    pub padding_left: f64,
}

impl DesktopSelectionGesture {
    fn decode(self) -> Result<SelectionGestureEvent> {
        let phase = integer::<u32>(self.phase)?;
        let handle = gesture_handle(phase, self.handle.as_deref())?;
        let [columns, cell_width, screen_height, padding_left] = self.geometry()?;
        Ok(SelectionGestureEvent {
            phase,
            handle,
            clicks: integer(self.clicks)?,
            column: integer(self.column)?,
            rectangle: self.rectangle,
            row: integer(self.row)?,
            x: finite(self.x)?,
            y: finite(self.y)?,
            columns,
            cell_width,
            screen_height,
            padding_left,
        })
    }

    fn geometry(&self) -> Result<[u32; 4]> {
        Ok([
            integer(self.columns)?,
            integer(self.cell_width)?,
            integer(self.screen_height)?,
            integer(self.padding_left)?,
        ])
    }
}

fn gesture_handle(phase: u32, value: Option<&str>) -> Result<u64> {
    match (phase, value) {
        (0, None) => Ok(0),
        (1 | 2, Some(value)) => handle(value),
        _ => Err(Error::from_reason("InvalidGesturePhaseOrHandle")),
    }
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopGestureResult {
    pub handle: String,
    pub start: Option<String>,
    pub end: Option<String>,
}

#[napi]
#[allow(
    clippy::needless_pass_by_value,
    reason = "NAPI requires owned JS values"
)]
impl DesktopClient {
    /// Subscribe without requesting geometry changes. Zero means no attach was
    /// queued (already admitted or not negotiated). Existing subscriptions keep
    /// their original policy; a session attach retains its viewport semantics.
    #[napi]
    pub fn attach_terminal_preserving_geometry(&self, terminal: String) -> Result<u32> {
        Ok(self
            .client()?
            .attach_terminal_preserving_geometry(&terminal_id(&terminal)?))
    }

    #[napi]
    pub fn create_view(&self, terminal: String) -> Result<String> {
        self.client()?
            .create_view(&terminal_id(&terminal)?)
            .map(|view| view.get().to_string())
            .map_err(engine_error)
    }

    #[napi]
    pub fn destroy_view(&self, view: String) -> Result<()> {
        self.client()?
            .destroy_view(view_id(&view)?)
            .map_err(engine_error)
    }

    /// Relative rows; negative scrolls toward history. Safe integers only.
    #[napi]
    pub fn scroll_view(&self, view: String, rows: f64) -> Result<()> {
        self.client()?
            .scroll_view(view_id(&view)?, Scroll::Delta(integer(rows)?))
            .map_err(engine_error)
    }

    #[napi]
    pub fn follow_live_view(&self, view: String) -> Result<()> {
        self.client()?
            .follow_live_view(view_id(&view)?)
            .map_err(engine_error)
    }

    /// Explicitly request canonical PTY geometry for this view's terminal.
    /// The app's focused-input owner decides when to call this; view creation
    /// and background layout do not resize automatically. Queued means only
    /// queued: read subsequent viewInfo metadata for authoritative dimensions.
    #[napi]
    pub fn resize_view(&self, view: String, cols: f64, rows: f64) -> Result<DesktopResizeOutcome> {
        resize_view(&self.client()?, &view, cols, rows)
    }

    #[napi]
    pub fn view_info(&self, view: String) -> Result<DesktopViewInfo> {
        let frame = self
            .client()?
            .acquire_view(view_id(&view)?)
            .ok_or_else(|| Error::from_reason("StaleViewOrProjectionUnavailable"))?;
        Ok(DesktopViewInfo {
            view_id: view,
            terminal_id: crate::projection::id::encode(&frame.terminal_id),
            generation: frame.generation.to_string(),
            stream_id: frame.stream_id.to_string(),
            bootstrap_id: frame.bootstrap_id.to_string(),
            last_seq: frame.last_seq.to_string(),
            cols: frame.cols,
            rows: frame.rows,
            scroll_total: frame.scrollbar.total.to_string(),
            scroll_offset: frame.scrollbar.offset.to_string(),
            scroll_length: frame.scrollbar.len.to_string(),
            at_tail: frame.scrollbar.at_tail(),
        })
    }

    #[napi]
    pub fn track_view_anchor(&self, view: String, point: DesktopDocumentPoint) -> Result<String> {
        engine(&self.client()?)?
            .track_view_anchor(view_id(&view)?, point.decode()?)
            .map(|id| id.to_string())
            .map_err(engine_error)
    }

    #[napi]
    pub fn release_view_anchor(&self, view: String, anchor: String) -> Result<()> {
        engine(&self.client()?)?
            .release_view_anchor(view_id(&view)?, handle(&anchor)?)
            .map_err(engine_error)
    }

    #[napi]
    pub fn pin_viewport_view(&self, view: String, anchor: String) -> Result<()> {
        self.client()?
            .pin_viewport_view(view_id(&view)?, handle(&anchor)?)
            .map_err(engine_error)
    }

    #[napi]
    pub fn set_view_selection(
        &self,
        view: String,
        start: String,
        end: String,
        rectangle: bool,
    ) -> Result<()> {
        engine(&self.client()?)?
            .set_view_selection(view_id(&view)?, handle(&start)?, handle(&end)?, rectangle)
            .map_err(engine_error)
    }

    #[napi]
    pub fn clear_view_selection(&self, view: String) -> Result<()> {
        engine(&self.client()?)?
            .clear_view_selection(view_id(&view)?)
            .map_err(engine_error)
    }

    /// Copy at most one MiB through the engine's bounded formatter. Its
    /// conservative 1,048,576-cell work cap may also refuse large selections.
    /// Unsupported or over-budget copies fail without an unbounded fallback.
    #[napi]
    pub fn view_selection_text(&self, view: String) -> Result<String> {
        let result = engine(&self.client()?)?
            .selection_text_view_bounded(view_id(&view)?, SELECTION_BYTE_LIMIT)
            .map_err(engine_error)?;
        selection_text(result)
    }

    /// Search at most 4096 query bytes. The runtime caps results at 4096 and
    /// replaces previous search handles, preserving active selection endpoints.
    #[napi]
    pub fn search_view(
        &self,
        view: String,
        query: String,
        case_sensitive: bool,
    ) -> Result<Vec<DesktopSearchMatch>> {
        bounded_text(&query, 4096)?;
        let found = engine(&self.client()?)?
            .search_view(view_id(&view)?, query, case_sensitive)
            .map_err(engine_error)?;
        Ok(found
            .into_iter()
            .map(|hit| DesktopSearchMatch {
                start: hit.start.to_string(),
                end: hit.end.to_string(),
            })
            .collect())
    }

    #[napi]
    pub fn view_selection_gesture(
        &self,
        view: String,
        event: DesktopSelectionGesture,
    ) -> Result<DesktopGestureResult> {
        let result = engine(&self.client()?)?
            .view_selection_gesture(view_id(&view)?, event.decode()?)
            .map_err(engine_error)?;
        Ok(DesktopGestureResult {
            handle: result.handle.to_string(),
            start: (result.start != 0).then(|| result.start.to_string()),
            end: (result.end != 0).then(|| result.end.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_copy_outcomes_never_fall_back_or_expand_invalid_utf8() {
        for (outcome, reason) in [
            (BoundedSelectionText::Unavailable, "SelectionUnavailable"),
            (
                BoundedSelectionText::ByteLimitExceeded,
                "SelectionByteLimitExceeded",
            ),
            (
                BoundedSelectionText::WorkLimitExceeded,
                "SelectionWorkLimitExceeded",
            ),
            (
                BoundedSelectionText::Unsupported,
                "BoundedSelectionUnsupported",
            ),
        ] {
            assert_eq!(selection_text(outcome).expect_err("refused").reason, reason);
        }
        assert_eq!(
            selection_text(BoundedSelectionText::Text("é🙂".as_bytes().to_vec())).expect("text"),
            "é🙂"
        );
        assert_eq!(
            selection_text(BoundedSelectionText::Text(vec![0xff]))
                .expect_err("invalid UTF8")
                .reason,
            "InvalidSelectionUtf8"
        );
        assert!(
            selection_text(BoundedSelectionText::Text(vec![
                b'x';
                SELECTION_BYTE_LIMIT + 1
            ]))
            .is_err()
        );
    }

    #[test]
    fn numbers_are_checked_before_narrowing() {
        for value in [f64::NAN, f64::INFINITY, -1.0, 0.5, 65536.0, 4_294_967_296.0] {
            assert!(integer::<u16>(value).is_err());
        }
        assert_eq!(integer::<u16>(65535.0).ok(), Some(u16::MAX));
        assert_eq!(integer::<u32>(4_294_967_295.0).ok(), Some(u32::MAX));
        assert!(integer::<i64>(9_007_199_254_740_992.0).is_err());
        assert_eq!(integer::<i64>(-42.0).ok(), Some(-42));
    }

    #[test]
    fn handles_are_lossless_canonical_nonzero_strings() {
        assert_eq!(handle("18446744073709551615").ok(), Some(u64::MAX));
        for value in ["0", "01", "+1", "1.5", "1e3", " 1", "18446744073709551616"] {
            assert!(handle(value).is_err());
        }
    }
}
