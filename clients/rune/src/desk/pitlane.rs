//! The pit lane view: the fleet as race cars, machines as garages in the
//! galaxy's order.

use std::cell::RefCell;

use super::Desk;
use super::playground::{Playground, Scene, creatures};
use crate::pitlane;

/// Its paint: the grounds by day and by night, drawn once per layout (the
/// layout's revision and the two shapes).
pub type PitLane = Playground<pitlane::PitLane, RefCell<(u64, rune::Shape, rune::Shape)>>;

impl Scene for pitlane::PitLane {
    type Paint = RefCell<(u64, rune::Shape, rune::Shape)>;

    fn of(desk: &mut Desk) -> &mut PitLane {
        &mut desk.pitlane
    }

    fn refresh(desk: &mut Desk) {
        let (hosts, drivers) = desk.cast(false);
        let v = &mut desk.pitlane;
        v.scene.sync(drivers);
        let [w, h] = v.viewport;
        v.scene.layout(w, h, &hosts);
    }

    creatures!(cars);
}
