use super::*;
use crate::grid::GridError;
use libghostty_vt::{Error, render::RowIterator, terminal::ScrollViewport};

#[test]
fn failed_snapshot_inspection_forces_a_full_retry() {
    let mut terminal = seeded_terminal(4, 2, b"old");
    let mut projector = GridProjector::new().unwrap();
    projector.project(&terminal, 1).unwrap();
    terminal.vt_write(b"\rnew");
    let failed = projector.project_with_fringe_and_snapshot(&terminal, 1, None, |_| {
        Err::<(), _>(GridError::Engine(Error::OutOfMemory))
    });
    assert!(matches!(failed, Err(GridError::Engine(Error::OutOfMemory))));
    let retried = projector.project(&terminal, 1).unwrap();
    assert_eq!(retried.damage, GridDamage::Full);
    assert!(retried.buffer.row_dirty.iter().all(|dirty| *dirty));
    assert_eq!(retried.buffer.cell_text(0), b"n");
    assert_eq!(retried.buffer.cells.len(), 8);
}

#[test]
fn overscan_is_visible_to_the_hook_but_not_the_dense_viewport() {
    let mut terminal = seeded_terminal(4, 2, b"0\r\n1\r\n2\r\n3");
    terminal.scroll_viewport(ScrollViewport::Top);
    let mut projector = GridProjector::new().unwrap();
    let mut rows = RowIterator::new().unwrap();
    // A changed generation rebuilds the render state; the overscan request
    // must survive that rebuild as well as an ordinary subsequent update.
    for generation in [1, 1, 2] {
        let (grid, ys) = projector
            .project_with_fringe_and_snapshot(&terminal, generation, Some(2), |snapshot| {
                assert_eq!(snapshot.overscan()?.below, 1);
                let mut iteration = rows.update(snapshot)?;
                let mut ys = Vec::new();
                while let Some(row) = iteration.next() {
                    ys.push(row.viewport_y()?);
                }
                Ok(ys)
            })
            .unwrap();
        assert_eq!(ys, [0, 1, 2]);
        assert_eq!(grid.buffer.cells.len(), 8);
        assert_eq!(grid.buffer.row_dirty.len(), 2);
        assert_eq!(grid.buffer.fringe.len(), 4);
        assert_eq!(grid.buffer.fringe_text(0), b"2");
    }
    let (grid, below) = projector
        .project_with_fringe_and_snapshot(&terminal, 2, None, |snapshot| {
            Ok(snapshot.overscan()?.below)
        })
        .unwrap();
    assert_eq!(below, 0);
    assert!(grid.buffer.fringe.is_empty());
}
