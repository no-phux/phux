//! The floating plugin overlay pane (ADR-0147).
//!
//! A manifest `[[panes]]` entry with `placement = "overlay"` spawns an
//! ordinary server Terminal that this client never adds to a layout window.
//! Its [`PaneSlot`] carries `floating = Some(title)`, and while it exists the
//! client presents it as a modal box centered over the pane area:
//!
//! - it takes keyboard input, paste, and pointer events inside its box;
//! - the panes beneath stop painting (like any modal) and catch up when it
//!   closes;
//! - any resolved action dismisses it first (killing its Terminal), and
//!   `kill-pane` does nothing else;
//! - it closes when its process exits.
//!
//! Nothing here touches the wire: the spawn is the same `SPAWN_RESOURCE` a
//! split sends, and the dismissal is an ordinary `KILL_RESOURCE`.

use std::collections::HashMap;
use std::io::{self, Write};

use phux_protocol::ids::ResourceId;

use crate::attach::pane_state::PaneSlot;
use crate::layout::Rect;
use crate::render::Theme;

/// Share of the pane area each box axis takes.
const BOX_SHARE_PERCENT: u32 = 80;
/// Smallest box the share is allowed to shrink to (border included), when
/// the pane area can hold it.
const MIN_BOX: (u16, u16) = (24, 8);

/// The box a floating pane occupies: `outer` with its one-cell border,
/// `inner` the Terminal's own cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::attach) struct FloatingBox {
    /// The border rectangle.
    pub(in crate::attach) outer: Rect,
    /// The Terminal's cells, `outer` inset by one on every side.
    pub(in crate::attach) inner: Rect,
}

/// Center the box in `content`: 80% of each axis, at least [`MIN_BOX`],
/// never larger than `content`.
#[must_use]
pub(in crate::attach) fn floating_box(content: Rect) -> FloatingBox {
    let axis = |len: u16, min: u16| -> u16 {
        let share = u32::from(len) * BOX_SHARE_PERCENT / 100;
        u16::try_from(share).unwrap_or(len).max(min).min(len)
    };
    let w = axis(content.w, MIN_BOX.0);
    let h = axis(content.h, MIN_BOX.1);
    let outer = Rect {
        x: content.x + (content.w - w) / 2,
        y: content.y + (content.h - h) / 2,
        w,
        h,
    };
    let inner = Rect {
        x: outer.x.saturating_add(1).min(outer.x + outer.w),
        y: outer.y.saturating_add(1).min(outer.y + outer.h),
        w: outer.w.saturating_sub(2),
        h: outer.h.saturating_sub(2),
    };
    FloatingBox { outer, inner }
}

/// The open floating pane, if any.
#[must_use]
pub(in crate::attach) fn floating_pane(
    panes: &HashMap<ResourceId, PaneSlot>,
) -> Option<&ResourceId> {
    panes
        .iter()
        .find_map(|(id, slot)| slot.floating.is_some().then_some(id))
}

/// The pane that owns the screen's focus for painting: the floating pane
/// while one is open, else the layout's focused leaf.
#[must_use]
pub(in crate::attach) fn paint_focus<'a>(
    panes: &'a HashMap<ResourceId, PaneSlot>,
    focused: Option<&'a ResourceId>,
) -> Option<&'a ResourceId> {
    floating_pane(panes).or(focused)
}

/// Where a pane with no tile in the render window paints: the box interior
/// for the floating pane, else the whole content rect (the single-pane
/// bootstrap before a layout lands).
#[must_use]
pub(in crate::attach) fn untiled_rect(
    panes: &HashMap<ResourceId, PaneSlot>,
    id: &ResourceId,
    content: Rect,
) -> Rect {
    if panes.get(id).is_some_and(|slot| slot.floating.is_some()) {
        floating_box(content).inner
    } else {
        content
    }
}

/// Whether `(x, y)` (outer-viewport cells) lies inside `rect`.
#[must_use]
pub(in crate::attach) const fn rect_contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.w)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.h)
}

/// Draw the box border with `title` set into its top edge, in the focused
/// divider tone. Clears the interior only through the pane paint that
/// follows; the caller paints the Terminal into `inner` afterwards.
///
/// # Errors
/// Forwards any `io::Error` from `out`.
pub(in crate::attach) fn paint_floating_frame<W: Write>(
    out: &mut W,
    frame: FloatingBox,
    title: &str,
    theme: &Theme,
) -> io::Result<()> {
    let FloatingBox { outer, .. } = frame;
    if outer.w < 2 || outer.h < 2 {
        return Ok(());
    }
    let inner_w = usize::from(outer.w - 2);
    out.write_all(b"\x1b[0m")?;
    crate::render::write_sgr_color(out, theme.divider_focus, true)?;
    crate::render::write_sgr_color(out, theme.surface, false)?;
    let label = if title.is_empty() || inner_w < 4 {
        String::new()
    } else {
        format!(" {} ", crate::render::clip_text(title, inner_w - 2))
    };
    let label_w = crate::render::display_width(&label);
    let top = format!("┌{label}{}┐", "─".repeat(inner_w.saturating_sub(label_w)));
    crate::attach::render::write_cup(out, outer.y, outer.x)?;
    out.write_all(top.as_bytes())?;
    for row in 1..outer.h - 1 {
        crate::attach::render::write_cup(out, outer.y + row, outer.x)?;
        out.write_all("│".as_bytes())?;
        crate::attach::render::write_cup(out, outer.y + row, outer.x + outer.w - 1)?;
        out.write_all("│".as_bytes())?;
    }
    let bottom = format!("└{}┘", "─".repeat(inner_w));
    crate::attach::render::write_cup(out, outer.y + outer.h - 1, outer.x)?;
    out.write_all(bottom.as_bytes())?;
    out.write_all(b"\x1b[0m")
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    const fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn the_box_centers_at_eighty_percent_inside_the_content() {
        let b = floating_box(rect(30, 1, 100, 40));
        assert_eq!(b.outer, rect(40, 5, 80, 32));
        assert_eq!(b.inner, rect(41, 6, 78, 30));
    }

    #[test]
    fn small_content_keeps_the_minimum_then_clamps_to_the_content() {
        let b = floating_box(rect(0, 0, 26, 9));
        assert_eq!(b.outer, rect(1, 0, 24, 8));
        let tiny = floating_box(rect(2, 3, 10, 4));
        assert_eq!(tiny.outer, rect(2, 3, 10, 4));
        assert_eq!(tiny.inner, rect(3, 4, 8, 2));
        let degenerate = floating_box(rect(0, 0, 1, 1));
        assert_eq!(degenerate.inner.w, 0);
        assert_eq!(degenerate.inner.h, 0);
    }

    #[test]
    fn the_floating_slot_owns_paint_focus_and_paints_in_its_box() {
        let content = rect(0, 1, 100, 40);
        let layout_pane = ResourceId::local(1);
        let overlay = ResourceId::local(9);
        let mut panes = HashMap::new();
        panes.insert(layout_pane.clone(), PaneSlot::new().expect("slot"));
        assert_eq!(floating_pane(&panes), None);
        assert_eq!(paint_focus(&panes, Some(&layout_pane)), Some(&layout_pane));
        assert_eq!(untiled_rect(&panes, &layout_pane, content), content);

        let mut slot = PaneSlot::new().expect("slot");
        slot.floating = Some("Board".to_owned());
        panes.insert(overlay.clone(), slot);
        assert_eq!(floating_pane(&panes), Some(&overlay));
        assert_eq!(paint_focus(&panes, Some(&layout_pane)), Some(&overlay));
        assert_eq!(
            untiled_rect(&panes, &overlay, content),
            floating_box(content).inner
        );
    }

    #[test]
    fn the_frame_draws_a_titled_border_around_the_interior() {
        let mut out = Vec::new();
        let frame = floating_box(rect(0, 0, 30, 10));
        paint_floating_frame(&mut out, frame, "Agent Board", &Theme::default()).expect("paint");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("┌ Agent Board ─"), "{text:?}");
        assert!(text.contains('┐') && text.contains('└') && text.contains('┘'));
        assert_eq!(
            text.matches('│').count(),
            usize::from(frame.outer.h - 2) * 2
        );
    }
}
