//! The Dock icon's count. Rune has no call for it, so this is the crate's
//! one piece of unsafe: a label on `NSApp.dockTile`.

/// Show `count` on the Dock icon; zero clears it. Headless runs have no tile.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn badge(count: usize) {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, msg_send_id};
    use objc2_foundation::NSString;

    let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
    // Safety: `NSApp` and its Dock tile live for the process, and both calls
    // are made on the main thread (Rune's loop, from `poll`).
    unsafe {
        let app: Retained<AnyObject> = msg_send_id![class!(NSApplication), sharedApplication];
        let tile: Retained<AnyObject> = msg_send_id![&app, dockTile];
        let label: *const NSString = label.as_deref().map_or(std::ptr::null(), |l| l);
        let _: () = msg_send![&tile, setBadgeLabel: label];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn badge(_count: usize) {}
