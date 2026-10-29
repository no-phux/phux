//! Retained-row movement, scroll search, fringe rows, and cursor invalidation.

mod tests {
    use super::super::*;

    #[test]
    fn a_scrolled_screen_keeps_the_rows_that_moved() {
        let settings = Settings::default();
        let previous = row_grid(&["aaa", "bbb", "ccc"]);
        let next = row_grid(&["bbb", "ccc", "ddd"]);
        assert_eq!(scroll_of(&previous, &next), Some(Scroll::Up(1)));
        assert_eq!(
            rows_after_scroll(
                &previous,
                &next,
                &settings,
                (Scroll::Up(1), previous.rows),
                &scrolling_geometry()
            ),
            vec![2]
        );

        let previous = cursor_at(previous, CursorStyle::Block, 2, 0);
        let next = cursor_at(next, CursorStyle::Block, 2, 0);
        assert_eq!(
            rows_after_scroll(
                &previous,
                &next,
                &settings,
                (Scroll::Up(1), previous.rows),
                &scrolling_geometry()
            ),
            vec![1, 2]
        );

        let back = row_grid(&["aaa", "bbb", "ccc"]);
        let forward = row_grid(&["zzz", "aaa", "bbb"]);
        assert_eq!(scroll_of(&back, &forward), Some(Scroll::Down(1)));
        assert_eq!(
            rows_after_scroll(
                &back,
                &forward,
                &settings,
                (Scroll::Down(1), back.rows),
                &scrolling_geometry()
            ),
            vec![0]
        );
        assert!(scroll_of(&back, &row_grid(&["axa", "bbb", "ccc"])).is_none());
    }

    #[test]
    fn scroll_search_runs_only_when_most_rows_changed() {
        assert!(!worth_scroll_search(1, 40));
        assert!(!worth_scroll_search(34, 40));
        assert!(worth_scroll_search(35, 40));
        assert!(worth_scroll_search(40, 40));
        assert!(worth_scroll_search(3, 3));
        assert!(!worth_scroll_search(2, 3));
        assert!(!worth_scroll_search(0, 1));
    }

    #[test]
    fn shifting_a_scene_moves_the_kept_row() {
        let geometry = Geometry {
            cell_width: px(8.),
            cell_height: px(10.),
            scale: 1.,
            cols: 3,
            rows: 3,
            ..Default::default()
        };
        let mut scene = builder(
            row_grid(&["aaa", "bbb", "ccc"]),
            Settings::default(),
            geometry,
        )
        .scene;
        let origin = geometry.cell_bounds(2, 0, 1);
        scene.backgrounds.push(RowQuad {
            row: 2,
            bounds: origin,
            color: gpui::red(),
            bend: None,
        });
        let height = scene.frame.rows;
        shift_scene(&mut scene, Scroll::Up(1), height);
        assert_eq!(scene.backgrounds.len(), 1);
        assert_eq!(scene.backgrounds[0].row, 1);
        assert_eq!(
            scene.backgrounds[0].bounds.origin.y,
            geometry.cell_bounds(1, 0, 1).origin.y
        );
    }

    #[test]
    fn scrolling_reuses_only_exact_rows_in_both_directions_and_at_history_limit() {
        let settings = Settings::default();
        let geometry = scrolling_geometry();
        let old = scrolling_frame(&["A", "B", "C", "D"], 10);
        let mut next = scrolling_frame(&["B", "C", "D", "E"], 11);
        assert_eq!(scroll_of(&old, &next), Some(Scroll::Up(1)));
        assert_eq!(
            rows_after_scroll(&old, &next, &settings, (Scroll::Up(1), old.rows), &geometry),
            vec![3]
        );
        assert_eq!(scroll_of(&next, &old), Some(Scroll::Down(1)));
        assert_eq!(
            rows_after_scroll(
                &next,
                &old,
                &settings,
                (Scroll::Down(1), next.rows),
                &geometry
            ),
            vec![0]
        );
        next.scrollbar.offset = old.scrollbar.offset;
        assert_eq!(
            scroll_of(&old, &next),
            Some(Scroll::Up(1)),
            "history at capacity"
        );
        next.scrollbar.offset += 1;
        next.buffer.cells[2].flags |= CELL_SELECTED;
        next.buffer.cells[4].wide = 1;
        assert_eq!(
            rows_after_scroll(&old, &next, &settings, (Scroll::Up(1), old.rows), &geometry),
            vec![1, 2, 3],
            "selection and wide-head changes must not keep stale ink"
        );
        let clipped = Geometry {
            bounds: Bounds::new(geometry.bounds.origin, size(px(100.), px(30.))),
            ..geometry
        };
        assert_eq!(
            rows_after_scroll(&old, &next, &settings, (Scroll::Up(1), old.rows), &clipped),
            vec![0, 1, 2, 3],
            "previously clipped rows must be freshly painted when exposed"
        );
    }

    /// A row that leaves the viewport becomes the fringe and the old fringe
    /// enters it, so a one-row history scroll shapes only the row new to the
    /// scene.
    #[test]
    fn history_scrolling_moves_the_fringe_with_the_viewport() {
        let settings = Settings::default();
        let geometry = scrolling_geometry();
        // Older history above: scrolling back moves content down.
        let near = with_fringe(scrolling_frame(&["B", "C", "D", "E"], 11), "F");
        let back = with_fringe(scrolling_frame(&["A", "B", "C", "D"], 10), "E");
        assert_eq!(shifted_height(&near, &back), 5);
        assert_eq!(scroll_of(&near, &back), Some(Scroll::Down(1)));
        assert_eq!(
            rows_after_scroll(&near, &back, &settings, (Scroll::Down(1), 5), &geometry),
            vec![0],
            "the row leaving the bottom is the new fringe"
        );
        assert_eq!(
            rows_after_scroll(&back, &near, &settings, (Scroll::Up(1), 5), &geometry),
            vec![4],
            "the old fringe enters the viewport"
        );
        let other = with_fringe(scrolling_frame(&["A", "B", "C", "D"], 10), "X");
        assert_eq!(
            rows_after_scroll(&near, &other, &settings, (Scroll::Down(1), 5), &geometry),
            vec![0, 4],
            "a fringe that does not match its source row is shaped again"
        );
        assert_eq!(
            shifted_height(&scrolling_frame(&["A", "B", "C", "D"], 10), &back),
            4,
            "a frame without a fringe shifts the viewport alone"
        );
    }

    #[test]
    fn scrolling_invalidates_old_and_new_solid_cursor_ink() {
        let old = cursor_at(
            scrolling_frame(&["A", "B", "C", "D"], 0),
            CursorStyle::Block,
            1,
            0,
        );
        let next = cursor_at(
            scrolling_frame(&["B", "C", "D", "E"], 1),
            CursorStyle::Block,
            2,
            0,
        );
        assert_eq!(
            rows_after_scroll(
                &old,
                &next,
                &Settings::default(),
                (Scroll::Up(1), old.rows),
                &scrolling_geometry()
            ),
            vec![0, 2, 3]
        );
    }

    #[test]
    fn retained_primitives_move_together_on_the_shared_pixel_grid() {
        let geometry = scrolling_geometry();
        let mut scene = builder(
            scrolling_frame(&["A", "B", "C"], 0),
            Settings::default(),
            geometry,
        )
        .scene;
        let bounds = geometry.cell_bounds(1, 0, 2);
        scene.backgrounds.push(RowQuad {
            row: 1,
            bounds,
            color: gpui::red(),
            bend: None,
        });
        scene.decorations.push(RowQuad {
            row: 1,
            bounds,
            color: gpui::blue(),
            bend: None,
        });
        scene.glyphs.push(GlyphObservation {
            row: 1,
            col: 0,
            bounds,
            origin: bounds.origin,
            baseline: px(35.),
            foreground: gpui::red(),
        });
        let height = scene.frame.rows;
        shift_scene(&mut scene, Scroll::Up(1), height);
        assert_eq!(scene.backgrounds[0].row, 0);
        assert_eq!(scene.backgrounds[0].bounds, geometry.cell_bounds(0, 0, 2));
        assert_eq!(scene.decorations[0].bounds, scene.backgrounds[0].bounds);
        assert_eq!(scene.glyphs[0].row, 0);
        assert_eq!(scene.glyphs[0].origin.y, px(0.));
        assert_eq!(scene.glyphs[0].baseline, px(15.));
    }
}
