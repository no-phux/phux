use super::{
    Format, Formatter, FormatterOptions, GhosttyTerminal, GridRef, Point, PointCoordinate,
    RENDER_BUDGET_BYTES, RENDERED_FORMAT_HTML, RENDERED_FORMAT_VT, ROW_WINDOW_MAX, RenderedScreen,
    SCROLLBACK_ALL, Selection, SnapshotSynthesizer, SynthesisError, history_window_start,
};
use base64::Engine as _;

impl<'alloc> SnapshotSynthesizer<'alloc> {
    /// Render through libghostty's Formatter for `GET_SCREEN`'s `rendered`
    /// field. `format`'s low bits select the output (`0` none, `1` HTML,
    /// `2` VT; the caller has already refused others) and the high bit
    /// joins soft-wrapped rows.
    ///
    /// A [`Selection`] is always built: without one the Formatter emits the
    /// whole scrollback, ignoring `scrollback` and the budget. History is
    /// clamped to [`ROW_WINDOW_MAX`] rows, and a capture over
    /// `RENDER_BUDGET_BYTES` is refused with
    /// [`SynthesisError::RenderBudgetExceeded`] rather than truncated.
    #[allow(
        clippy::unused_self,
        reason = "kept as a method on SnapshotSynthesizer for API symmetry \
                  with screen_state_with_scrollback, matching its own \
                  intentionally-stateless rationale"
    )]
    pub fn render_screen(
        &self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
        format: u8,
    ) -> Result<Option<RenderedScreen>, SynthesisError> {
        Self::render_screen_with_budget(terminal, scrollback, format, RENDER_BUDGET_BYTES)
    }

    /// [`Self::render_screen`] with an explicit budget (a test seam).
    pub(super) fn render_screen_with_budget(
        terminal: &GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
        format: u8,
        budget: usize,
    ) -> Result<Option<RenderedScreen>, SynthesisError> {
        let selector = format & phux_protocol::wire::frame::GET_SCREEN_FORMAT_SELECTOR_MASK;
        let unwrap = format & phux_protocol::wire::frame::GET_SCREEN_FORMAT_UNWRAP != 0;
        let target = match selector {
            1 => Format::Html,
            2 => Format::Vt,
            _ => return Ok(None),
        };
        let selection = Self::render_selection(terminal, scrollback)?;
        let mut formatter = Formatter::new(
            terminal,
            FormatterOptions::new()
                .with_format(target)
                .with_trim(true)
                .with_unwrap(unwrap)
                .with_selection(&selection),
        )?;
        let required = formatter.format_len()?;
        if required > budget {
            return Err(SynthesisError::RenderBudgetExceeded { required, budget });
        }
        let bytes = formatter.format_alloc(None)?;
        let (tag, data) = if selector == 1 {
            (
                RENDERED_FORMAT_HTML,
                String::from_utf8_lossy(&bytes).into_owned(),
            )
        } else {
            (
                RENDERED_FORMAT_VT,
                base64::engine::general_purpose::STANDARD.encode(&bytes),
            )
        };
        Ok(Some(RenderedScreen {
            format: tag.to_owned(),
            data,
        }))
    }

    /// The selection [`Self::render_screen`] passes: from as far into history
    /// as `scrollback` asks (else the viewport's top-left) to the viewport's
    /// bottom-right.
    fn render_selection<'t>(
        terminal: &'t GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
    ) -> Result<Selection<'t>, SynthesisError> {
        let cols = terminal.cols()?;
        let rows = terminal.rows()?;
        let end = terminal.grid_ref(Point::Viewport(PointCoordinate {
            x: cols.saturating_sub(1),
            y: u32::from(rows.saturating_sub(1)),
        }))?;
        let start = Self::render_selection_start(terminal, scrollback)?;
        Ok(Selection::new(start, end, false))
    }

    /// The start endpoint for [`Self::render_selection`], clamped to
    /// [`ROW_WINDOW_MAX`] history rows so `Some(0)` cannot hand the
    /// Formatter the whole scrollback on the single-threaded runtime.
    fn render_selection_start<'t>(
        terminal: &'t GhosttyTerminal<'alloc, '_>,
        scrollback: Option<u32>,
    ) -> Result<GridRef<'t>, SynthesisError> {
        let viewport_origin = Point::Viewport(PointCoordinate { x: 0, y: 0 });
        let Some(want) = scrollback else {
            return Ok(terminal.grid_ref(viewport_origin)?);
        };
        let total = terminal.scrollback_rows()?;
        if total == 0 {
            return Ok(terminal.grid_ref(viewport_origin)?);
        }
        let bounded_want = if want == SCROLLBACK_ALL {
            ROW_WINDOW_MAX
        } else {
            want.min(ROW_WINDOW_MAX)
        };
        let start_row = history_window_start(total, bounded_want);
        Ok(terminal.grid_ref(Point::History(PointCoordinate {
            x: 0,
            y: u32::try_from(start_row).unwrap_or(u32::MAX),
        }))?)
    }
}
