//! Background, font, colour, cursor, and procedural-block regressions.

mod tests {
    use super::super::*;

    #[test]
    fn backgrounds_merge_into_row_runs_and_skip_the_default() {
        let geometry = Geometry {
            cell_width: px(10.),
            cell_height: px(20.),
            scale: 1.,
            ..Default::default()
        };
        let mut prepared = builder(frame(), Settings::default(), geometry);
        let red: Hsla = gpui::rgb(0xff0000).into();
        let blue: Hsla = gpui::rgb(0x0000ff).into();
        let default = prepared.scene.background;
        // Row 0: default, red, red, blue, default. Row 1: red continues no run.
        for (col, color) in [default, red, red, blue, default].into_iter().enumerate() {
            prepared.background_cell(0, col as u16, color);
        }
        prepared.background_cell(1, 5, red);
        prepared.background_cell(1, 7, red);
        prepared.flush_background();
        let quads: Vec<_> = prepared
            .scene
            .backgrounds
            .iter()
            .map(|quad| {
                (
                    quad.bounds.origin.x,
                    quad.bounds.origin.y,
                    quad.bounds.size.width,
                    quad.color,
                )
            })
            .collect();
        assert_eq!(
            quads,
            vec![
                (px(10.), px(0.), px(20.), red),
                (px(30.), px(0.), px(10.), blue),
                (px(50.), px(20.), px(10.), red),
                (px(70.), px(20.), px(10.), red),
            ]
        );
    }

    #[test]
    fn styles_index_the_four_faces() {
        let fonts = Fonts::new(&gpui::font("Menlo"));
        let face = |flags| {
            &fonts.cell[style(&Cell {
                flags,
                ..Default::default()
            })]
        };
        assert_eq!(face(0).weight, gpui::FontWeight::default());
        assert_eq!(face(CELL_BOLD).weight, gpui::FontWeight::BOLD);
        assert_eq!(face(CELL_ITALIC).style, gpui::FontStyle::Italic);
        let both = face(CELL_BOLD | CELL_ITALIC);
        assert_eq!(
            (both.weight, both.style),
            (gpui::FontWeight::BOLD, gpui::FontStyle::Italic)
        );
        assert!(
            fonts
                .run
                .iter()
                .all(|font| font.features != fonts.cell[0].features)
        );
    }

    #[test]
    fn inverse_swaps_true_color_before_selection_and_faint() {
        let frame = frame();
        let settings = Settings::default();
        let mut cell = Cell {
            foreground_r: 255,
            background_b: 255,
            flags: CELL_INVERSE,
            ..Default::default()
        };
        let metadata = CellMetadata {
            foreground_kind: 2,
            ..Default::default()
        };
        let colors = cell_colors(
            &cell,
            metadata,
            &frame,
            &settings.theme(),
            defaults(&frame, &settings),
        );
        assert_eq!(colors.foreground, gpui::rgb(0x0000ff).into());
        assert_eq!(colors.background, gpui::rgb(0xff0000).into());
        cell.flags |= CELL_SELECTED | CELL_FAINT;
        let colors = cell_colors(
            &cell,
            metadata,
            &frame,
            &settings.theme(),
            defaults(&frame, &settings),
        );
        assert_eq!(colors.background, settings.selection_background);
        assert_eq!(
            colors.foreground.a, 1.,
            "glyph color must not double-dim window opacity"
        );
        assert_eq!(colors.opacity, 0.5);
    }

    #[test]
    fn theme_uses_provenance_and_respects_terminal_default_override() {
        let mut frame = frame();
        let settings = Settings {
            foreground: Some(gpui::rgb(0xabcdef).into()),
            background: Some(gpui::rgb(0x123456).into()),
            ..Default::default()
        };
        let cell = Cell::default();
        let metadata = CellMetadata {
            background_color_is_default: true,
            underline_color_is_default: true,
            ..Default::default()
        };
        let colors = cell_colors(
            &cell,
            metadata,
            &frame,
            &settings.theme(),
            defaults(&frame, &settings),
        );
        assert_eq!(colors.foreground, settings.foreground.expect("foreground"));
        assert_eq!(colors.underline, colors.foreground);
        frame.colors.reversed = true;
        let colors = cell_colors(
            &cell,
            metadata,
            &frame,
            &settings.theme(),
            defaults(&frame, &settings),
        );
        assert_eq!(colors.foreground, settings.background.expect("background"));
        frame.colors.has_foreground = true;
        frame.colors.foreground = Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(
            cell_colors(
                &cell,
                metadata,
                &frame,
                &settings.theme(),
                defaults(&frame, &settings)
            )
            .foreground,
            gpui::rgb(0xff0000).into()
        );
    }

    #[test]
    fn hidden_and_blinking_cells_do_not_produce_glyphs() {
        let mut settings = Settings::default();
        assert!(hidden(
            &Cell {
                flags: CELL_INVISIBLE,
                ..Default::default()
            },
            &settings
        ));
        let blinking = Cell {
            flags: CELL_BLINK,
            ..Default::default()
        };
        assert!(!hidden(&blinking, &settings));
        settings.blink_visible = false;
        assert!(hidden(&blinking, &settings));
    }

    #[test]
    fn wide_tail_cursor_uses_head_and_unfocused_cursor_is_not_solid() {
        let mut frame = frame();
        frame.cursor = Cursor {
            visible: true,
            col: 4,
            row: 1,
            width: CursorWidth::WideTail,
            style: CursorStyle::Block,
            blinking: false,
        };
        assert_eq!(cursor_head(&frame), (1, 3));
        assert_eq!(cursor_span(&frame), (1, 3, 2));
        frame.cursor.width = CursorWidth::Wide;
        assert_eq!(cursor_span(&frame), (1, 4, 2));
        frame.cursor.width = CursorWidth::Narrow;
        assert_eq!(cursor_span(&frame), (1, 4, 1));
        assert!(solid_cursor(&frame, &Settings::default()));
        assert!(!solid_cursor(
            &frame,
            &Settings {
                focused: false,
                ..Default::default()
            }
        ));
    }

    #[test]
    fn half_blocks_share_the_exact_device_pixel_boundary() {
        for scale in [1., 1.25, 1.5, 2.] {
            let bounds = Bounds::new(
                point(px(2. / scale), px(3. / scale)),
                size(px(13. / scale), px(27. / scale)),
            );
            let upper = block_bounds(bounds, block_regions("▀".as_bytes()).unwrap()[0], scale);
            let lower = block_bounds(bounds, block_regions("▄".as_bytes()).unwrap()[0], scale);
            assert_eq!(upper.bottom(), lower.top());
            assert_eq!(upper.top(), bounds.top());
            assert_eq!(lower.bottom(), bounds.bottom());
            assert_eq!(upper.size.width, bounds.size.width);
            assert!(
                block_regions("▀\u{fe0f}".as_bytes()).is_none(),
                "a grapheme with selectors must keep normal text shaping"
            );
        }
    }
}
