//! Where a menu trigger was last painted, so its menu drops from the trigger
//! instead of a hard-coded corner.
//!
//! A trigger adds [`Anchors::probe`] as a child of a relative element; the
//! probe records that element's window bounds on every prepaint. Chrome is
//! cached between desk notifications, and a cached view keeps its last
//! bounds, which stay right until the window resizes and repaints it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Bounds, IntoElement, Pixels, SharedString, canvas};

#[derive(Clone, Default)]
pub(crate) struct Anchors(Rc<RefCell<HashMap<SharedString, Bounds<Pixels>>>>);

impl Anchors {
    /// Bounds last painted for `key`, in window coordinates.
    pub(crate) fn get(&self, key: &str) -> Option<Bounds<Pixels>> {
        self.0.borrow().get(key).copied()
    }

    /// An invisible child that records its parent's bounds under `key`.
    pub(crate) fn probe(&self, key: impl Into<SharedString>) -> impl IntoElement {
        let (map, key) = (self.0.clone(), key.into());
        canvas(
            move |bounds, _, _| {
                map.borrow_mut().insert(key, bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}
