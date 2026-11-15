//! The kitchen view: the fleet as chefs, machines as lines in a steady order.

use std::cell::RefCell;

use super::Desk;
use super::playground::{Playground, Scene, creatures};
use crate::kitchen;

pub type Kitchen = Playground<kitchen::Kitchen, Paint>;

#[derive(Debug, Default)]
pub struct Paint {
    /// The room's static paint and the layout generation it was made for.
    pub room: RefCell<(u32, rune::Shape)>,
    /// The night's darkness, redrawn each frame while it's dark.
    pub mask: rune::Pixels,
}

impl Scene for kitchen::Kitchen {
    type Paint = Paint;

    fn of(desk: &mut Desk) -> &mut Kitchen {
        &mut desk.kitchen
    }

    fn refresh(desk: &mut Desk) {
        let (lines, cooks) = desk.cast(true);
        let v = &mut desk.kitchen;
        let [w, h] = v.viewport;
        v.scene.layout(w, h, &lines);
        v.scene.sync(cooks);
    }

    creatures!(chefs);
}
