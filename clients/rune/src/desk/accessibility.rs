//! The system's display preferences Rune has no API for: reduce
//! transparency and the text size (§1.9, §1.4 "Text scaling").
//!
//! - macOS: `NSWorkspace.accessibilityDisplayShouldReduceTransparency`,
//!   read at launch and again whenever AppKit posts
//!   `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification`, which
//!   wakes the window.
//! - iOS: the host bridge reports `UIAccessibility.isReduceTransparencyEnabled`
//!   and the `UIContentSizeCategory` through [`Desk::set_accessibility`].
//! - Desktop text size: the chrome's own `TextSize` preference, one factor
//!   over every chrome role. The grid font has its own setting.
//!
//! `Tokens` hashes both into `tokens.version`, so a memo keyed on it
//! repaints when either changes.

use rune::prefs::Prefs;

use super::Desk;

/// The chrome text sizes the desktop steps through: 100% to 140%, the
/// largest the gallery checks.
pub const TEXT_SCALES: [f32; 5] = [1.0, 1.1, 1.2, 1.3, 1.4];

/// Where the chrome text size is remembered.
const TEXT_SCALE_PREF: &str = "chrome_text_scale";

/// Chrome next to the terminal grid grows at most this much on a phone
/// (`ChromeScale.gridCapped`, mobile's §1.4 rule).
pub const GRID_CAP: f32 = 1.35;

/// iOS's text-size setting, `UIContentSizeCategory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentSize {
    ExtraSmall,
    Small,
    Medium,
    /// The system default.
    #[default]
    Large,
    ExtraLarge,
    ExtraExtraLarge,
    ExtraExtraExtraLarge,
    AccessibilityMedium,
    AccessibilityLarge,
    AccessibilityExtraLarge,
    AccessibilityExtraExtraLarge,
    AccessibilityExtraExtraExtraLarge,
}

impl ContentSize {
    /// The category UIKit names `raw` (`UIContentSizeCategory.rawValue`,
    /// `UICTContentSizeCategoryL` and so on); anything else is the default.
    pub fn from_category(raw: &str) -> Self {
        let size = raw.strip_prefix("UICTContentSizeCategory").unwrap_or(raw);
        match size {
            "XS" => Self::ExtraSmall,
            "S" => Self::Small,
            "M" => Self::Medium,
            "XL" => Self::ExtraLarge,
            "XXL" => Self::ExtraExtraLarge,
            "XXXL" => Self::ExtraExtraExtraLarge,
            "AccessibilityM" => Self::AccessibilityMedium,
            "AccessibilityL" => Self::AccessibilityLarge,
            "AccessibilityXL" => Self::AccessibilityExtraLarge,
            "AccessibilityXXL" => Self::AccessibilityExtraExtraLarge,
            "AccessibilityXXXL" => Self::AccessibilityExtraExtraExtraLarge,
            _ => Self::Large,
        }
    }

    /// The body text factor `UIFontMetrics` applies at this size: the body
    /// style's point size here over its 17 points at the default.
    pub fn factor(self) -> f32 {
        let body: f32 = match self {
            Self::ExtraSmall => 14.0,
            Self::Small => 15.0,
            Self::Medium => 16.0,
            Self::Large => 17.0,
            Self::ExtraLarge => 19.0,
            Self::ExtraExtraLarge => 21.0,
            Self::ExtraExtraExtraLarge => 23.0,
            Self::AccessibilityMedium => 28.0,
            Self::AccessibilityLarge => 33.0,
            Self::AccessibilityExtraLarge => 40.0,
            Self::AccessibilityExtraExtraLarge => 47.0,
            Self::AccessibilityExtraExtraExtraLarge => 53.0,
        };
        body / 17.0
    }

    /// The factor for chrome beside the terminal grid, capped (§1.4).
    pub fn grid_capped(self) -> f32 {
        self.factor().min(GRID_CAP)
    }
}

/// What the system says about transparency and text size.
#[derive(Debug, Clone, PartialEq)]
pub struct Accessibility {
    reduce_transparency: bool,
    content_size: ContentSize,
    /// The desktop chrome's text size, one of `TEXT_SCALES`.
    text_scale: f32,
    /// The display-options change count last read (macOS); `None` before
    /// the first read.
    read: Option<u64>,
}

impl Accessibility {
    pub(super) fn load(prefs: &Prefs) -> Self {
        Self {
            reduce_transparency: false,
            content_size: ContentSize::default(),
            text_scale: snap(prefs.get::<f32>(TEXT_SCALE_PREF).unwrap_or(1.0)),
            read: None,
        }
    }
}

/// `scale` as the nearest step the setting offers.
fn snap(scale: f32) -> f32 {
    let distance = |s: &&f32| (**s - scale).abs();
    let nearest = TEXT_SCALES
        .iter()
        .min_by(|a, b| distance(a).total_cmp(&distance(b)));
    nearest.copied().unwrap_or(1.0)
}

impl Desk {
    /// Glass draws opaque: the system asks for less transparency.
    pub const fn reduce_transparency(&self) -> bool {
        self.accessibility.reduce_transparency
    }

    /// The phone's text-size category, as the host last reported it.
    pub const fn content_size(&self) -> ContentSize {
        self.accessibility.content_size
    }

    /// The desktop chrome's text size.
    pub const fn text_scale(&self) -> f32 {
        self.accessibility.text_scale
    }

    /// The one factor over every chrome type role (`Tokens::type_scale`):
    /// the phone's content size, capped beside the grid, else the
    /// desktop's text-size setting.
    pub fn type_scale(&self) -> f32 {
        if self.phone.is_some() {
            self.content_size().grid_capped()
        } else {
            self.text_scale()
        }
    }

    /// Choose the chrome text size; it snaps to a step and is remembered.
    /// True when it changed.
    pub fn set_text_scale(&mut self, scale: f32) -> bool {
        let scale = snap(scale);
        if scale == self.accessibility.text_scale {
            return false;
        }
        self.accessibility.text_scale = scale;
        let _ = self.prefs.set(TEXT_SCALE_PREF, &scale);
        self.generation += 1;
        true
    }

    /// The phone host's report: Reduce Transparency and the
    /// `UIContentSizeCategory` raw value. True when either changed, so the
    /// bridge runs the view.
    pub fn set_accessibility(&mut self, reduce_transparency: bool, content_size: &str) -> bool {
        let size = ContentSize::from_category(content_size);
        let changed = self.set_reduce_transparency(reduce_transparency)
            | (size != self.accessibility.content_size);
        if changed {
            self.accessibility.content_size = size;
            self.generation += 1;
        }
        changed
    }

    /// The system's reduce-transparency setting; true when it changed.
    pub(crate) fn set_reduce_transparency(&mut self, on: bool) -> bool {
        if on == self.accessibility.reduce_transparency {
            return false;
        }
        self.accessibility.reduce_transparency = on;
        self.generation += 1;
        true
    }

    /// Read the platform again when it said something changed (macOS), and
    /// listen for the next change. True when what it says changed. Tests,
    /// headless runs and the phone (whose host reports) read nothing.
    pub(super) fn follow_accessibility(&mut self) -> bool {
        if cfg!(test) || self.native.is_some() {
            return false;
        }
        let Some((changes, reduce)) = platform::read(self.waker.as_ref(), self.accessibility.read)
        else {
            return false;
        };
        self.accessibility.read = Some(changes);
        self.set_reduce_transparency(reduce)
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod platform {
    use std::cell::{Cell, OnceCell, RefCell};

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel};
    use objc2_foundation::NSString;
    use rune::Waker;

    /// AppKit's name for the notification, `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification`.
    const CHANGED: &str = "NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification";

    thread_local! {
        /// The observer AppKit holds weakly, set once on the main thread.
        static OBSERVER: OnceCell<Retained<Observer>> = const { OnceCell::new() };
        /// Display-option changes AppKit has posted since launch.
        static CHANGES: Cell<u64> = const { Cell::new(0) };
        /// Each window's waker, woken by a change.
        static WAKERS: RefCell<Vec<Waker>> = const { RefCell::new(Vec::new()) };
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "PhuxRuneDisplayOptions"]
        struct Observer;

        impl Observer {
            #[unsafe(method(displayOptionsChanged:))]
            fn display_options_changed(&self, _note: &AnyObject) {
                CHANGES.set(CHANGES.get() + 1);
                WAKERS.with_borrow(|wakers| wakers.iter().for_each(|wake| wake()));
            }
        }
    );

    /// Start listening, once per process, on the main thread.
    fn observe(mtm: MainThreadMarker) {
        OBSERVER.with(|cell| {
            if cell.get().is_some() {
                return;
            }
            let observer: Retained<Observer> = unsafe { msg_send![Observer::alloc(mtm), init] };
            let name = NSString::from_str(CHANGED);
            // Safety: the shared workspace and its notification center live
            // for the process; the observer is kept in `OBSERVER` for as
            // long, and this runs on the main thread (checked by `mtm`).
            unsafe {
                let workspace: Retained<AnyObject> =
                    msg_send![class!(NSWorkspace), sharedWorkspace];
                let center: Retained<AnyObject> = msg_send![&workspace, notificationCenter];
                let _: () = msg_send![
                    &center,
                    addObserver: &*observer,
                    selector: sel!(displayOptionsChanged:),
                    name: &*name,
                    object: std::ptr::null::<AnyObject>()
                ];
            }
            let _ = cell.set(observer);
        });
    }

    /// The change count and the setting, when it may differ from what was
    /// last `read`. Registers `waker` for the next change.
    pub(super) fn read(waker: Option<&Waker>, read: Option<u64>) -> Option<(u64, bool)> {
        let mtm = MainThreadMarker::new()?;
        observe(mtm);
        if let Some(waker) = waker {
            WAKERS.with_borrow_mut(|wakers| {
                if !wakers.iter().any(|w| std::sync::Arc::ptr_eq(w, waker)) {
                    wakers.push(waker.clone());
                }
            });
        }
        let changes = CHANGES.get();
        if read == Some(changes) {
            return None;
        }
        // Safety: a documented `NSWorkspace` getter, on the main thread.
        let reduce: bool = unsafe {
            let workspace: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
            msg_send![&workspace, accessibilityDisplayShouldReduceTransparency]
        };
        Some((changes, reduce))
    }
}

/// Elsewhere the desktop has no read: transparency stays as it is.
#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn read(_waker: Option<&rune::Waker>, _read: Option<u64>) -> Option<(u64, bool)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;
    use crate::ui::tokens::{Inputs, Tokens};

    fn desk() -> Desk {
        let mut desk = Desk::new(&Resolved::offline("a11y"));
        desk.prefs = Prefs::memory();
        desk
    }

    /// The tokens a window draws with for what `desk` read.
    fn version(desk: &Desk) -> u64 {
        Tokens::resolve(&Inputs {
            reduce_transparency: desk.reduce_transparency(),
            type_scale: desk.type_scale(),
            ..Inputs::system(true)
        })
        .version
    }

    #[test]
    fn the_reduce_transparency_toggle_changes_the_tokens_version() {
        let mut desk = desk();
        let before = version(&desk);
        assert!(desk.set_reduce_transparency(true));
        let reduced = version(&desk);
        assert_ne!(before, reduced, "memos keyed on the version repaint");
        assert!(!desk.set_reduce_transparency(true), "no change twice");
        assert!(desk.set_reduce_transparency(false));
        assert_eq!(version(&desk), before, "back to the same tokens");
    }

    #[test]
    fn the_phone_host_reports_transparency_and_text_size() {
        let mut desk = desk().with_phone(true);
        let before = version(&desk);
        assert!(desk.set_accessibility(true, "UICTContentSizeCategoryAccessibilityXXXL"));
        assert!(desk.reduce_transparency());
        assert_eq!(
            desk.content_size(),
            ContentSize::AccessibilityExtraExtraExtraLarge
        );
        assert_eq!(desk.type_scale(), GRID_CAP, "capped beside the grid");
        assert_ne!(version(&desk), before);
        assert!(!desk.set_accessibility(true, "UICTContentSizeCategoryAccessibilityXXXL"));
        assert!(desk.set_accessibility(true, "UICTContentSizeCategoryM"));
        assert!(desk.type_scale() < 1.0, "smaller than the default");
    }

    #[test]
    fn content_sizes_follow_uikit_body_sizes() {
        assert_eq!(
            ContentSize::from_category("UICTContentSizeCategoryL").factor(),
            1.0
        );
        assert_eq!(ContentSize::from_category("nonsense"), ContentSize::Large);
        assert_eq!(ContentSize::from_category("XS"), ContentSize::ExtraSmall);
        let xl = ContentSize::from_category("UICTContentSizeCategoryXL").factor();
        assert!((xl - 19.0 / 17.0).abs() < 1e-6);
        assert!(ContentSize::AccessibilityMedium.factor() > GRID_CAP);
        assert_eq!(ContentSize::AccessibilityMedium.grid_capped(), GRID_CAP);
    }

    #[test]
    fn the_desktop_text_size_snaps_and_is_remembered() {
        let mut desk = desk();
        assert_eq!(desk.type_scale(), 1.0);
        assert!(!desk.set_text_scale(1.0), "already the smallest");
        let before = version(&desk);
        assert!(desk.set_text_scale(1.1));
        assert_eq!(desk.text_scale(), 1.1);
        assert_ne!(version(&desk), before, "the type roles grew");
        assert!(desk.set_text_scale(9.0), "snaps to the largest");
        assert_eq!(desk.type_scale(), 1.4);
        assert!(!desk.set_text_scale(1.4));
        let remembered = Accessibility::load(&desk.prefs);
        assert_eq!(remembered.text_scale, 1.4);
    }

    #[test]
    fn tests_never_read_the_machine_they_run_on() {
        let mut desk = desk();
        assert!(!desk.follow_accessibility());
        assert!(!desk.reduce_transparency());
    }
}
