//! Keeping the grant sheet clear of System Settings: after Grant…, the sheet
//! looks for Settings' window and moves this one so the two sit side by side,
//! and puts it back once the grant comes through or the sheet closes.

use std::time::Duration;

use rune::Tx;

use crate::desk::Desk;
use crate::system_settings;

/// How long after Grant… the sheet waits for System Settings to dock beside.
const DOCK_FOR: f64 = 5.0;
/// How often it looks meanwhile.
const LOOK_EVERY: Duration = Duration::from_millis(250);

/// The window, to move the grant sheet beside System Settings; the sheet's
/// last rect in global and in window points; until when on the app clock
/// it's looking for Settings; and where the window was before it moved, in
/// physical pixels, to put it back.
pub struct Dock {
    pub window: Option<rune::AppHandle<Desk>>,
    pub sheet_at: [f32; 4],
    pub sheet_in_window: [f32; 4],
    until: f64,
    home: Option<(i32, i32)>,
}

impl Default for Dock {
    fn default() -> Self {
        Self {
            window: None,
            sheet_at: [0.0; 4],
            sheet_in_window: [0.0; 4],
            until: f64::NEG_INFINITY,
            home: None,
        }
    }
}

impl Desk {
    /// Grant… was pressed: look for Settings' window for a while.
    pub(super) fn start_dock(&mut self) {
        self.onboarding.dock.until = self.clock + DOCK_FOR;
    }

    /// From the grant sheet's poll: dock once Settings has a window, or look
    /// again soon.
    pub(super) fn tick_dock(&mut self, tx: &mut Tx) {
        if self.clock >= self.onboarding.dock.until {
            return;
        }
        if self.dock_sheet() {
            self.onboarding.dock.until = f64::NEG_INFINITY;
        } else {
            tx.wake_after(LOOK_EVERY);
        }
    }

    /// Move the window so the grant sheet sits beside System Settings, its
    /// drag item clear of the list it's dragged into. True once Settings has
    /// a window, moved or not.
    fn dock_sheet(&mut self) -> bool {
        let Some((settings, screen)) = system_settings::frame() else {
            return false;
        };
        let scale = self.scale.get();
        let dock = &mut self.onboarding.dock;
        if let (Some(window), Some(x)) = (
            &dock.window,
            system_settings::beside(dock.sheet_at, settings, screen),
        ) {
            // Full-size content: the content's origin is the window's.
            let [gx, gy, ..] = dock.sheet_at;
            let [wx, wy, ..] = dock.sheet_in_window;
            let px = |v: f32| (v * scale).round() as i32;
            dock.home.get_or_insert((px(gx - wx), px(gy - wy)));
            window.set_position((px(x - wx), px(gy - wy)));
        }
        true
    }

    /// Put the window back where it was before it moved beside Settings.
    pub(super) fn undock(&mut self) {
        let dock = &mut self.onboarding.dock;
        dock.until = f64::NEG_INFINITY;
        if let (Some(window), Some(home)) = (&dock.window, dock.home.take()) {
            window.set_position(home);
        }
    }
}
