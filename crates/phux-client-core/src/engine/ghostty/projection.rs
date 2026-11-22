//! Document point conversion and grapheme reads shared by search and selection.

use super::{DocumentPoint, DocumentSpace, GhosttyEngineError, Point, PointCoordinate, PointSpace};

pub(super) const fn to_ghostty_point(point: DocumentPoint) -> Point {
    let coordinate = PointCoordinate {
        x: point.x,
        y: point.y,
    };
    match point.space {
        DocumentSpace::History => Point::History(coordinate),
        DocumentSpace::Viewport => Point::Viewport(coordinate),
        DocumentSpace::Active => Point::Active(coordinate),
    }
}

pub(super) const fn to_ghostty_space(space: DocumentSpace) -> PointSpace {
    match space {
        DocumentSpace::History => PointSpace::History,
        DocumentSpace::Viewport => PointSpace::Viewport,
        DocumentSpace::Active => PointSpace::Active,
    }
}

pub(super) fn grid_ref_graphemes(
    grid_ref: &libghostty_vt::grid_ref::GridRef<'_>,
) -> Result<Vec<char>, GhosttyEngineError> {
    let mut inline = ['\0'; 8];
    match grid_ref.graphemes(&mut inline) {
        Ok(len) => Ok(inline[..len].to_vec()),
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            let mut values = vec!['\0'; required];
            let len = grid_ref.graphemes(&mut values)?;
            values.truncate(len);
            Ok(values)
        }
        Err(error) => Err(error.into()),
    }
}
