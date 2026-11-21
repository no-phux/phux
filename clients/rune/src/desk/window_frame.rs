//! The main window's frame across launches, and the display a new window
//! opens on (`[rune] new-window-screen`).

use std::cell::RefCell;

use phux_config_core::rune::NewWindowScreen;
use rune::prefs::Prefs;
use rune::{Align, DisplayTarget, OverlayPlacement, ScreenEdge, WindowHandle, WindowPlacement};
use serde::{Deserialize, Serialize};

const KEY: &str = "window_frame";

/// A window placed on a display, rather than restored, opens this far below
/// its top edge (clear of a menu bar), centred across it.
const FROM_TOP: f32 = 64.0;

thread_local! {
    /// The main window, from its creation, for the frame its close saves.
    static MAIN: RefCell<Option<WindowHandle>> = const { RefCell::new(None) };
}

/// Where the main window was when it closed: its content size in points and
/// its frame's top-left in physical desktop pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub size: (f32, f32),
    pub position: (i32, i32),
}

impl Frame {
    /// The frame to reopen at; none while minimized or full screen.
    fn of(at: &WindowPlacement) -> Option<Self> {
        if at.minimized || at.full_screen || at.scale <= 0.0 {
            return None;
        }
        let (position, (w, h)) = at.restore.unwrap_or((at.position, at.content_size));
        let points = |px: u32| (f64::from(px) / at.scale) as f32;
        Some(Self {
            size: (points(w), points(h)),
            position,
        })
    }
}

/// The frame the last close saved.
pub fn saved() -> Option<Frame> {
    Prefs::open(super::PREFS_ID).get(KEY)
}

impl super::Desk {
    /// Remember the main window's frame for the next launch.
    pub fn save_frame(&mut self) {
        let placement = MAIN.with_borrow(|main| main.as_ref().and_then(WindowHandle::placement));
        if let Some(frame) = placement.as_ref().and_then(Frame::of) {
            let _ = self.prefs.set(KEY, &frame);
        }
    }
}

/// The main window was created: keep its handle, and move it to the main
/// display if the one it was restored to is gone.
pub fn opened(window: &WindowHandle) {
    if window.placement().is_some_and(|at| !on_display(&at)) {
        place(window, NewWindowScreen::Main);
    }
    MAIN.set(Some(window.clone()));
}

/// Open centred near the top of the display `screen` names. `Current` keeps
/// the restored position.
pub fn place(window: &WindowHandle, screen: NewWindowScreen) {
    let display = match screen {
        NewWindowScreen::Current => return,
        NewWindowScreen::Mouse => DisplayTarget::UnderPointer,
        NewWindowScreen::Main => DisplayTarget::Primary,
    };
    let at = OverlayPlacement::Edge {
        edge: ScreenEdge::Top,
        margin: FROM_TOP,
        align: Align::Center,
        inset: 0.0,
    };
    window.place(display, at);
}

/// Whether the window's middle is on the display it reports; a window
/// restored to a display that is gone is on none.
fn on_display(at: &WindowPlacement) -> bool {
    let Some(((x, y), (w, h))) = at.display else {
        return false;
    };
    let half = |px: u32| i64::from(px / 2);
    let middle = (
        i64::from(at.position.0) + half(at.content_size.0),
        i64::from(at.position.1) + half(at.content_size.1),
    );
    let (x, y) = (i64::from(x), i64::from(y));
    (x..x + i64::from(w)).contains(&middle.0) && (y..y + i64::from(h)).contains(&middle.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(position: (i32, i32), display: Option<((i32, i32), (u32, u32))>) -> WindowPlacement {
        WindowPlacement {
            position,
            content_size: (2200, 1440),
            scale: 2.0,
            display,
            ..WindowPlacement::default()
        }
    }

    #[test]
    fn a_frame_is_saved_in_points_and_found_off_a_display_that_is_gone() {
        let display = Some(((0, 0), (5120, 2880)));
        let frame = Frame::of(&at((100, 80), display)).expect("a frame");
        assert_eq!(
            frame,
            Frame {
                size: (1100.0, 720.0),
                position: (100, 80)
            }
        );
        assert!(on_display(&at((100, 80), display)));
        // Restored to a display left of this one that is unplugged.
        assert!(!on_display(&at((-3000, 80), display)));
        assert!(!on_display(&at((100, 80), None)));
        let full = WindowPlacement {
            full_screen: true,
            ..at((0, 0), display)
        };
        assert_eq!(Frame::of(&full), None);
    }
}
