//! Scene-cache reuse, damage comparison, patch fences, and row merging.

mod tests {
    use super::super::*;

    #[test]
    fn a_scene_is_reused_only_while_its_inputs_are_unchanged() {
        let settings = Settings::default();
        let geometry = Geometry {
            bounds: Bounds::new(point(px(4.), px(8.)), size(px(100.), px(40.))),
            ..Default::default()
        };
        let scene = builder(frame(), settings.clone(), geometry).scene;
        let same = Arc::clone(&scene.frame);
        let bounds = scene.bounds;
        assert!(scene.reusable(&same, bounds, (1., None), &settings));
        // Input policy and sizing do not change a prepared scene.
        let mut unpainted = settings.clone();
        unpainted.size_owner = false;
        unpainted.option_as_alt = true;
        unpainted.app_chords = vec!["ctrl+tab".into()];
        assert!(scene.reusable(&same, bounds, (1., None), &unpainted));
        assert!(
            !scene.reusable(&Arc::new(frame()), bounds, (1., None), &settings),
            "an equal but newly published frame is a new frame"
        );
        assert!(!scene.reusable(&same, bounds, (2., None), &settings));
        let moved = Bounds::new(point(px(5.), px(8.)), bounds.size);
        assert!(!scene.reusable(&same, moved, (1., None), &settings));
        let changes: [fn(&mut Settings); 6] = [
            |settings| settings.focused = false,
            |settings| settings.cursor_visible = false,
            |settings| settings.blink_visible = false,
            |settings| settings.foreground = Some(gpui::rgb(0x123456).into()),
            |settings| settings.font = gpui::font("Monaco"),
            |settings| settings.cell_width_scale = 1.1,
        ];
        for change in changes {
            let mut changed = settings.clone();
            change(&mut changed);
            assert!(!scene.reusable(&same, bounds, (1., None), &changed));
        }
    }

    #[test]
    fn a_row_matches_across_arena_offsets_until_flags_or_the_solid_cursor_change() {
        let settings = Settings::default();
        let mut left_utf8 = Vec::new();
        let mut right_utf8 = b"pad".to_vec();
        let left_cell = text_cell(&mut left_utf8, b"A");
        let right_cell = text_cell(&mut right_utf8, b"A");
        let blank = Cell::default();
        let left = painted_frame(vec![left_cell, blank, blank, blank], left_utf8);
        let mut right = painted_frame(vec![right_cell, blank, blank, blank], right_utf8);
        assert!(row_paints_alike(&left, &right, 0));
        assert_eq!(rows_to_rebuild(&left, &right, &settings), Vec::<u16>::new());

        right.buffer.cells[0].flags |= CELL_SELECTED;
        assert_eq!(rows_to_rebuild(&left, &right, &settings), vec![0]);
        right.buffer.cells[0].flags = 0;

        let bar_left = cursor_at(left, CursorStyle::Bar, 0, 0);
        let bar_right = cursor_at(right, CursorStyle::Bar, 1, 1);
        assert_eq!(
            rows_to_rebuild(&bar_left, &bar_right, &settings),
            Vec::<u16>::new()
        );
        assert_ne!(
            cursor_key(&bar_left, &settings),
            cursor_key(&bar_right, &settings)
        );

        let block_left = cursor_at(bar_left, CursorStyle::Block, 0, 0);
        let block_right = cursor_at(bar_right, CursorStyle::Block, 1, 0);
        assert_eq!(
            rows_to_rebuild(&block_left, &block_right, &settings),
            vec![0, 1]
        );
    }

    #[test]
    fn sparse_damage_keeps_changed_rows_but_skipped_publications_compare_the_whole_grid() {
        let settings = Settings::default();
        let previous = row_grid(&["aaaa", "bbbb", "cccc"]);
        let mut adjacent = row_grid(&["aaaa", "bBbb", "cccc"]);
        adjacent.generation = 2;
        adjacent.damage = GridDamage::Rows;
        adjacent.buffer.row_dirty = vec![false, true, false];
        assert_eq!(rows_to_rebuild(&previous, &adjacent, &settings), vec![1]);

        let mut skipped = row_grid(&["zaza", "bBbb", "cccc"]);
        skipped.generation = 3;
        skipped.damage = GridDamage::Rows;
        skipped.buffer.row_dirty = vec![false, true, false];
        assert_eq!(rows_to_rebuild(&previous, &skipped, &settings), vec![0, 1]);

        adjacent.damage = GridDamage::Full;
        adjacent.buffer.cells[0].flags |= CELL_SELECTED;
        assert_eq!(rows_to_rebuild(&previous, &adjacent, &settings), vec![0, 1]);
    }

    #[test]
    fn merging_rows_drops_the_rebuilt_row_and_keeps_order() {
        let quad = |row, color: Hsla| RowQuad {
            row,
            bounds: Bounds::default(),
            color,
            bend: None,
        };
        let fresh = quad(1, gpui::red());
        let merged = merge_rows(
            vec![
                quad(0, gpui::black()),
                quad(1, gpui::black()),
                quad(2, gpui::black()),
            ],
            vec![fresh],
            &[1],
            |item| item.row,
        );
        let rows: Vec<_> = merged.iter().map(|item| item.row).collect();
        assert_eq!(rows, vec![0, 1, 2]);
        assert_eq!(merged[1].color, fresh.color);
    }

    #[test]
    fn retained_rows_never_cross_replica_or_geometry_fences() {
        let geometry = Geometry {
            scale: 1.,
            ..Default::default()
        };
        let scene = builder(frame(), Settings::default(), geometry).scene;
        let mut next = frame();
        assert!(can_patch(
            &scene,
            &next,
            scene.bounds,
            (1., None),
            &scene.settings
        ));
        next.bootstrap_id += 1;
        assert!(!can_patch(
            &scene,
            &next,
            scene.bounds,
            (1., None),
            &scene.settings
        ));
        next.bootstrap_id -= 1;
        next.stream_id += 1;
        assert!(!can_patch(
            &scene,
            &next,
            scene.bounds,
            (1., None),
            &scene.settings
        ));
        next.stream_id -= 1;
        next.generation = 0;
        assert!(!can_patch(
            &scene,
            &next,
            scene.bounds,
            (1., None),
            &scene.settings
        ));
        next.generation = 2;
        assert!(!can_patch(
            &scene,
            &next,
            scene.bounds,
            (2., None),
            &scene.settings
        ));
    }
}
