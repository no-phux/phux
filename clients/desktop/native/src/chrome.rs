//! Titlebar drag for a window whose native titlebar is transparent.
//!
//! The app draws its own tab bar, so the empty parts of that bar must still
//! move the window and zoom it on double-click, as AppKit's titlebar does.

use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{
    App, Context, MouseButton, MouseDownEvent, Render, SharedString, Window, div, prelude::*,
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A region that drags the window and zooms it on double-click.
pub struct DragRegion {
    id: SharedString,
}

impl DragRegion {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            id: SharedString::from(format!("phux-drag-region-{id}")),
        }
    }
}

impl Render for DragRegion {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::fonts::install(cx);
        div()
            .id(self.id.clone())
            .size_full()
            .on_mouse_down(MouseButton::Left, drag_window)
    }
}

/// Native dragging and double-click behavior shared by header and canvas gutter.
pub(crate) fn drag_window(event: &MouseDownEvent, window: &mut Window, cx: &mut App) {
    if event.click_count >= 2 {
        window.titlebar_double_click();
    } else {
        window.start_window_move();
    }
    cx.stop_propagation();
}
