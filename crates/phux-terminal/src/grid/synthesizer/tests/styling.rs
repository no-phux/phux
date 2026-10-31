use super::*;

/// `(grapheme, fg, bg, underline, overline)` of a reconstructed cell.
type StyledCell = (char, Option<RgbColor>, Option<RgbColor>, bool, bool);

/// Per-cell styled view of the first row, for color round-trip asserts.
fn row0_styled(t: &GhosttyTerminal<'_, '_>) -> Vec<StyledCell> {
    let mut rs = RenderState::new().expect("RenderState::new");
    let snap = rs.update(t).expect("update");
    let mut row_iter_storage = RowIterator::new().expect("RowIterator::new");
    let mut cell_iter_storage = CellIterator::new().expect("CellIterator::new");
    let mut row_iter = row_iter_storage.update(&snap).expect("row update");
    let row = row_iter.next().expect("at least one row");
    let mut cell_iter = cell_iter_storage.update(row).expect("cell update");
    let mut out = Vec::new();
    while let Some(cell) = cell_iter.next() {
        let wide = cell.raw_cell().expect("raw_cell").wide().expect("wide");
        if matches!(wide, CellWide::SpacerTail) {
            continue;
        }
        let g = cell.graphemes().expect("graphemes");
        let ch = g.first().copied().unwrap_or(' ');
        let style = cell.style().expect("style");
        out.push((
            ch,
            cell.fg_color().expect("fg"),
            cell.bg_color().expect("bg"),
            !matches!(style.underline, libghostty_vt::style::Underline::None),
            style.overline,
        ));
    }
    out
}

/// Snapshot `vt`, replay it into a fresh terminal, return row 0's cells.
fn round_trip_row0(vt: &[u8]) -> Vec<StyledCell> {
    let mut source = fresh(40, 4);
    source.vt_write(vt);
    let snap = synthesize(&source).expect("synth");
    let mut client = fresh(40, 4);
    client.vt_write(&snap.bytes);
    row0_styled(&client)
}

/// A pure color change between same-attribute runs survives the snapshot.
#[test]
fn snapshot_preserves_adjacent_color_change() {
    let cells = round_trip_row0(b"\x1b[31mAB\x1b[34mCD\x1b[0m");
    let fg = |c: char| cells.iter().find(|x| x.0 == c).map(|x| x.1);
    assert_ne!(
        fg('A'),
        fg('C'),
        "red AB and blue CD must reconstruct as different foregrounds"
    );
    // Both runs are colored (neither collapsed to the default).
    assert!(fg('A').flatten().is_some(), "AB keeps a foreground");
    assert!(fg('C').flatten().is_some(), "CD keeps a foreground");
}

/// A colored blank region keeps its background through the snapshot.
#[test]
fn snapshot_preserves_blank_cell_background() {
    let cells = round_trip_row0(b"X\x1b[44m   \x1b[0mY");
    // Cells 1..=3 are blue-background spaces between X and Y.
    let blank_bg = cells[1].2;
    assert!(
        blank_bg.is_some(),
        "blue-background blanks must reconstruct with a background, got {blank_bg:?}"
    );
    assert_eq!(cells[1].0, ' ', "the colored region is blank");
    assert_eq!(cells[1].2, cells[2].2, "the whole blue run shares one bg");
}

/// Underline and overline must survive the snapshot — both emitters used
/// to drop them, flattening neovim undercurls and p10k underlined segments.
#[test]
fn snapshot_preserves_underline_and_overline() {
    // SGR 4 = underline, 53 = overline.
    let cells = round_trip_row0(b"\x1b[4mU\x1b[0m\x1b[53mO\x1b[0m");
    let u = cells.iter().find(|x| x.0 == 'U').expect("U cell");
    assert!(u.3, "underline must reconstruct");
    let o = cells.iter().find(|x| x.0 == 'O').expect("O cell");
    assert!(o.4, "overline must reconstruct");
}
