//! Where System Settings' window is, so the grant sheet can sit beside it
//! instead of under it. Rune lists no other app's windows, so this asks
//! CoreGraphics.

/// A gap between the sheet and Settings, in points.
const GAP: f32 = 12.0;

/// Where the sheet's left edge goes to sit clear of `settings` on `screen`:
/// to its left, else to its right, else at the screen's left edge, where the
/// drag item (on the sheet's left) still shows. None when they don't overlap.
/// Rects are `[x, y, w, h]` in global points, y down.
pub fn beside(sheet: [f32; 4], settings: [f32; 4], screen: [f32; 4]) -> Option<f32> {
    let [x, y, w, h] = sheet;
    let overlaps = x < settings[0] + settings[2]
        && settings[0] < x + w
        && y < settings[1] + settings[3]
        && settings[1] < y + h;
    if !overlaps {
        return None;
    }
    let left = settings[0] - GAP - w;
    let right = settings[0] + settings[2] + GAP;
    Some(if left >= screen[0] {
        left
    } else if right + w <= screen[0] + screen[2] {
        right
    } else {
        screen[0]
    })
}

/// System Settings' front window and the bounds of the display it's on, in
/// global points from the primary display's top-left, y down. None until it
/// has a window on screen.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn frame() -> Option<([f32; 4], [f32; 4])> {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    #[repr(C)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *mut AnyObject;
        fn CGRectMakeWithDictionaryRepresentation(dict: *const AnyObject, rect: *mut Rect) -> bool;
        fn CGGetDisplaysWithPoint(at: Point, max: u32, ids: *mut u32, count: *mut u32) -> i32;
        fn CGDisplayBounds(display: u32) -> Rect;
    }
    const ON_SCREEN_ONLY: u32 = 1;
    const EXCLUDE_DESKTOP: u32 = 1 << 4;
    let points = |r: Rect| [r.x as f32, r.y as f32, r.w as f32, r.h as f32];

    // Safety: CoreGraphics and AppKit queries with no shared state; the
    // window list is a +1 CFArray (toll-free an NSArray) that `Retained`
    // releases, and its entries are CFDictionaries of the documented keys.
    unsafe {
        let apps: Retained<AnyObject> = msg_send![
            AnyClass::get(c"NSRunningApplication")?,
            runningApplicationsWithBundleIdentifier: &*NSString::from_str("com.apple.systempreferences")
        ];
        let app: Option<Retained<AnyObject>> = msg_send![&apps, firstObject];
        let pid: i32 = msg_send![&app?, processIdentifier];
        let list = Retained::from_raw(CGWindowListCopyWindowInfo(
            ON_SCREEN_ONLY | EXCLUDE_DESKTOP,
            0,
        ))?;
        let key = |k: &str| NSString::from_str(k);
        let (owner, layer, bounds) = (
            key("kCGWindowOwnerPID"),
            key("kCGWindowLayer"),
            key("kCGWindowBounds"),
        );
        let count: usize = msg_send![&list, count];
        // Front to back: the first normal window of Settings' is its own.
        let window = (0..count).find_map(|i| {
            let info: Retained<AnyObject> = msg_send![&list, objectAtIndex: i];
            let int = |k: &NSString| -> Option<i32> {
                let n: Option<Retained<AnyObject>> = msg_send![&info, objectForKey: k];
                Some(msg_send![&n?, intValue])
            };
            if int(&owner)? != pid || int(&layer)? != 0 {
                return None;
            }
            let dict: Option<Retained<AnyObject>> = msg_send![&info, objectForKey: &*bounds];
            let mut rect = Rect::default();
            CGRectMakeWithDictionaryRepresentation(&*dict?, &mut rect).then_some(rect)
        })?;
        let mut display = 0;
        let mut found = 0;
        let centre = Point {
            x: window.x + window.w / 2.0,
            y: window.y + window.h / 2.0,
        };
        CGGetDisplaysWithPoint(centre, 1, &mut display, &mut found);
        (found > 0).then(|| (points(window), points(CGDisplayBounds(display))))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn frame() -> Option<([f32; 4], [f32; 4])> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: [f32; 4] = [0.0, 0.0, 1440.0, 900.0];

    #[test]
    fn the_sheet_moves_beside_settings_and_stays_on_screen() {
        let sheet = [480.0, 200.0, 480.0, 300.0];
        // Room on the left: the sheet ends a gap before Settings.
        let settings = [700.0, 100.0, 715.0, 600.0];
        assert_eq!(beside(sheet, settings, SCREEN), Some(700.0 - GAP - 480.0));
        // Settings at the left edge: the sheet goes to its right.
        let settings = [0.0, 100.0, 715.0, 600.0];
        assert_eq!(beside(sheet, settings, SCREEN), Some(715.0 + GAP));
        // No room either side: the screen's left edge, drag item showing.
        let settings = [400.0, 100.0, 715.0, 600.0];
        assert_eq!(beside(sheet, settings, SCREEN), Some(0.0));
        // Already clear of Settings: it stays.
        let settings = [1000.0, 100.0, 400.0, 600.0];
        assert_eq!(beside(sheet, settings, SCREEN), None);
        // A second display to the right counts from its own edge.
        let screen = [1440.0, 0.0, 1920.0, 1080.0];
        let sheet = [1900.0, 200.0, 480.0, 300.0];
        let settings = [1600.0, 100.0, 715.0, 600.0];
        assert_eq!(beside(sheet, settings, screen), Some(1600.0 + 715.0 + GAP));
    }
}
