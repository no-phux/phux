//! The sidebar's per-frame column reservation: full strip, rail, or none.

/// Which edge a reserved sidebar strip docks to (mirrors
/// `phux_config::SidebarPosition`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarEdge {
    /// Dock on the left; panes tile to its right.
    Left,
    /// Dock on the right; panes tile to its left.
    Right,
}

/// The sidebar's reservation: `width` columns on `edge`. Built once per frame
/// and threaded to every layout site so they agree on the inset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::attach) struct SidebarReservation {
    /// The edge the strip docks to.
    pub edge: SidebarEdge,
    /// Strip width in columns.
    pub width: u16,
}

/// Fold the sidebar's on/off state and geometry into the per-frame
/// reservation. The full strip needs `width + min_pane_cols` columns;
/// automatic width (`0`) is a quarter of the viewport clamped to `28..=40`.
/// Off, or too wide to fit, the strip collapses to the
/// [`RAIL_WIDTH`](crate::render::chrome::sidebar::RAIL_WIDTH) rail when
/// `rail` is set, else `None`.
pub(in crate::attach) const fn sidebar_reservation(
    outer_cols: u16,
    enabled: bool,
    rail: bool,
    width: u16,
    edge: SidebarEdge,
    min_pane_cols: u16,
) -> Option<SidebarReservation> {
    use crate::render::chrome::sidebar::RAIL_WIDTH;
    let width = resolved_sidebar_width(outer_cols, width);
    if enabled && strip_fits(outer_cols, width, min_pane_cols) {
        Some(SidebarReservation { edge, width })
    } else if rail && strip_fits(outer_cols, RAIL_WIDTH, min_pane_cols) {
        Some(SidebarReservation {
            edge,
            width: RAIL_WIDTH,
        })
    } else {
        None
    }
}

/// Whether the full strip (`width`, `0` automatic) fits beside
/// `min_pane_cols` of panes, so showing it changes the frame.
pub(in crate::attach) const fn full_sidebar_fits(
    outer_cols: u16,
    width: u16,
    min_pane_cols: u16,
) -> bool {
    strip_fits(
        outer_cols,
        resolved_sidebar_width(outer_cols, width),
        min_pane_cols,
    )
}

/// Automatic sizing follows the viewport, never changing titles or counts:
/// background activity must not reflow the terminal under someone's hands.
const fn resolved_sidebar_width(outer_cols: u16, width: u16) -> u16 {
    if width != 0 {
        return width;
    }
    let preferred = outer_cols / 4;
    if preferred < 28 {
        28
    } else if preferred > 40 {
        40
    } else {
        preferred
    }
}

const fn strip_fits(outer_cols: u16, width: u16, min_pane_cols: u16) -> bool {
    width <= outer_cols && outer_cols - width >= min_pane_cols
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::paint::{bar_inset, content_rect, sidebar_rect};
    use crate::render::ChromeBreakpoints;
    use crate::render::chrome::status_bar::Position;

    const LEFT20: SidebarReservation = SidebarReservation {
        edge: SidebarEdge::Left,
        width: 20,
    };

    /// The strip yields rather than starving the panes: reserved only when
    /// `width + min_pane_cols` fits; automatic width grows with the viewport
    /// (28..=40); the leftover content, strip, and bar inset always tile.
    #[test]
    fn the_sidebar_reserves_only_what_the_panes_can_afford() {
        let min = ChromeBreakpoints::DEFAULT.min_pane_cols;
        let res = |cols, width, min| {
            sidebar_reservation(cols, true, false, width, SidebarEdge::Left, min)
        };
        assert_eq!(res(59, 20, min), None);
        assert_eq!(res(60, 20, min), Some(LEFT20));
        assert!(
            res(50, 10, min).is_some(),
            "a narrower strip is affordable sooner"
        );
        assert_eq!(
            res(55, 20, 30),
            Some(LEFT20),
            "a lowered floor keeps the strip"
        );
        assert_eq!(res(70, 20, 60), None, "a raised floor takes it away");
        for cols in [0u16, 60, 200] {
            assert_eq!(
                sidebar_reservation(cols, false, false, 20, SidebarEdge::Left, min),
                None
            );
        }
        for cols in 0u16..=120 {
            let sidebar = res(cols, 20, min);
            let w = content_rect((cols, 24), Some(Position::Bottom), sidebar).w;
            assert!(
                if sidebar.is_some() {
                    w >= min
                } else {
                    w == cols
                },
                "cols={cols}"
            );
        }
        for edge in [SidebarEdge::Left, SidebarEdge::Right] {
            for (cols, expected) in [
                (67, None),
                (68, Some(28)),
                (80, Some(28)),
                (120, Some(30)),
                (144, Some(36)),
                (160, Some(40)),
                (240, Some(40)),
            ] {
                let res = sidebar_reservation(cols, true, false, 0, edge, 40);
                assert_eq!(res.map(|r| r.width), expected, "cols={cols}");
                if let Some(res) = res {
                    let content = content_rect((cols, 24), Some(Position::Bottom), Some(res));
                    assert!(content.w >= 40);
                    assert_eq!(content.w + sidebar_rect((cols, 24), res).w, cols);
                    assert_eq!(
                        bar_inset((cols, 24), Some(res)).span(cols),
                        (content.x, content.w)
                    );
                }
            }
            assert_eq!(
                sidebar_reservation(200, true, false, 22, edge, 40).map(|r| r.width),
                Some(22)
            );
            assert!(sidebar_reservation(u16::MAX, true, false, u16::MAX, edge, 40).is_none());
        }
    }

    /// Off, or too wide for the viewport, the strip collapses to the rail
    /// when `rail` is set; the rail yields to the pane floor like the strip.
    #[test]
    fn a_hidden_sidebar_keeps_the_rail() {
        use crate::render::chrome::sidebar::RAIL_WIDTH;
        let rail = Some(SidebarReservation {
            edge: SidebarEdge::Left,
            width: RAIL_WIDTH,
        });
        let res =
            |cols, enabled| sidebar_reservation(cols, enabled, true, 20, SidebarEdge::Left, 40);
        assert_eq!(res(80, false), rail, "toggled off keeps the rail");
        assert_eq!(res(80, true), Some(LEFT20), "on shows the full strip");
        assert_eq!(res(50, true), rail, "too narrow for the strip falls back");
        assert_eq!(res(42, false), None, "the rail yields to the pane floor");
        assert!(!full_sidebar_fits(50, 20, 40));
        assert!(full_sidebar_fits(60, 20, 40));
    }
}
