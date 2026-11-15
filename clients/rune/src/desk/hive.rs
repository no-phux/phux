//! The hive view: the fleet as bees, machines as frames in a steady order.

use std::cell::RefCell;

use super::Desk;
use super::playground::{Playground, Scene, creatures};
use crate::hive;

/// Its paint: the meadow, hive box and comb by day and by night, painted
/// once per layout generation.
pub type Hive = Playground<hive::Hive, RefCell<Option<(u64, rune::Shape, rune::Shape)>>>;

impl Scene for hive::Hive {
    type Paint = RefCell<Option<(u64, rune::Shape, rune::Shape)>>;

    fn of(desk: &mut Desk) -> &mut Hive {
        &mut desk.hive
    }

    fn refresh(desk: &mut Desk) {
        let (hosts, foragers) = desk.cast(true);
        let v = &mut desk.hive;
        let [w, h] = v.viewport;
        v.scene.layout(w, h, &hosts);
        v.scene.sync(foragers);
    }

    creatures!(bees);
}
