//! The quick terminal: a panel a global chord drops over whatever app is in
//! front, on the current Space, holding one terminal of its own session.
//!
//! It is a second window of this app with its own [`Desk`], always on this
//! computer's server. Showing it never activates the app or raises the main
//! window, so a window manager or Stage Manager doesn't switch away; it hides
//! again when it loses the keyboard. Its session is an ordinary one, so what
//! runs there outlives the panel and can be attached from anywhere. The main
//! window leaves that session out of its lists.

use phux_config_core::RuneCfg;
use phux_config_core::rune::QuickTerminalEdge;
use phux_protocol::ids::ResourceId;
use rune::{
    Align, AppHandle, DisplayTarget, OverlayPlacement, ScreenEdge, Tx, WindowOptions, WindowSpec,
};

use super::{Desk, NoticeKind};
use crate::launch::Resolved;

mod chord;

pub use chord::{Chord, refusal};

/// The session the panel shows, made again if its last terminal exits.
pub const SESSION: &str = "quick";

const SIZE: (f32, f32) = (960.0, 440.0);
const MIN_SIZE: (f32, f32) = (320.0, 160.0);

/// `quick-terminal-size` when unset, and the range it is held to.
const HEIGHT: (f32, f32, f32) = (0.4, 0.2, 0.9);

/// `NSWindowCollectionBehavior`: CanJoinAllSpaces | FullScreenAuxiliary, so
/// it opens over the Space, or the full-screen app, in front.
const ALL_SPACES: usize = 1 | 1 << 8;

/// The panel window's own part: its handle, the main window it hands a
/// terminal to, and whether it has dialed the server yet.
pub struct Panel {
    me: AppHandle<Desk>,
    main: AppHandle<Desk>,
    /// A panel off at launch connects the first time it's shown, so a chord
    /// nobody set never makes the session.
    dialed: bool,
}

/// Whether `session` is the quick terminal's, which the main window doesn't
/// list.
pub fn hidden(session: &str) -> bool {
    session == SESSION
}

impl Desk {
    /// The panel's window state: this computer's server and the quick session.
    fn quick(resolved: &Resolved, me: AppHandle<Self>, main: AppHandle<Self>) -> Self {
        let mut desk = Self::open(resolved, None);
        desk.launch.session = Some(SESSION.to_owned());
        let dialed = chord::at_launch(&desk.file, &desk.prefs).0;
        desk.quick = Some(Panel { me, main, dialed });
        // It opens hidden, so nothing of it has the keyboard yet.
        desk.focused = false;
        desk
    }

    /// A panel nobody has shown yet, which stays off the server.
    pub(super) fn quick_idle(&self) -> bool {
        self.quick.as_ref().is_some_and(|panel| !panel.dialed)
    }

    /// The panel's terminal moves into the main window's selected session as
    /// a new tab, and the main window comes forward on it. The quick session
    /// gets a fresh shell for the next time the panel shows.
    pub fn move_to_tab(&mut self) {
        let (Some(panel), Some(shown)) = (&self.quick, &self.shown) else {
            return;
        };
        let terminal = shown.resource.clone();
        panel.me.hide();
        panel
            .main
            .update(move |desk, _| desk.adopt_quick(&terminal));
        panel.main.open();
    }

    /// The main window takes `terminal` into its selected session as a tab.
    fn adopt_quick(&mut self, terminal: &ResourceId) {
        if self.machines.current.is_some() {
            return self.say(
                NoticeKind::Error,
                "This window is on another machine, so the quick terminal stays where it is.",
            );
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let session = client.selected_session().and_then(|id| {
            let topology = self.topology.as_ref()?;
            topology.sessions.iter().find(|s| s.id == id)?.key
        });
        let resource = client
            .organization()
            .resource_by_live(terminal)
            .map(|r| r.key);
        let (Some(session), Some(resource)) = (session, resource) else {
            return self.say(
                NoticeKind::Error,
                "Open a session here first, then move the quick terminal to it.",
            );
        };
        match client.move_resource(resource, session) {
            Ok(_) => {
                self.place_split(&client, terminal, None);
                if let Some(panel) = &self.quick_chord.panel {
                    panel.update(|desk, _| desk.refill_quick());
                }
            }
            Err(error) => self.say(
                NoticeKind::Error,
                format!("Couldn't move the quick terminal: {error}"),
            ),
        }
    }

    /// The panel's terminal became a tab: a fresh shell for next time.
    fn refill_quick(&mut self) {
        if self.selected_session_name().as_deref() == Some(SESSION) {
            self.new_terminal();
        }
    }
}

/// Add the panel to `app`. The main window registers its chord.
pub fn add(app: &mut rune::App<Desk>, resolved: Resolved) -> AppHandle<Desk> {
    let main = app.handle();
    let panel = rune::App::new(crate::app_spec(SIZE, MIN_SIZE)).windowless();
    let me = panel.handle();
    let panel = panel
        .init(move || Desk::quick(&resolved, me, main))
        .modifier_keys()
        .on_ime(|desk: &mut Desk, event, _| desk.ime(event))
        .on_focus(|desk: &mut Desk, focused, tx| {
            desk.focus_changed(focused);
            if !focused {
                tx.close_window();
            }
        });
    let options = WindowOptions::launcher_panel()
        .macos(|native| native.collection_behavior(ALL_SPACES).corner_radius(12.0));
    app.window(WindowSpec::from_app("quick", panel).configured_options(options))
        .0
}

/// The chord: hide the panel while it has the keyboard, else bring it in at
/// the configured edge of the display under the pointer.
fn toggle(desk: &mut Desk, _: &mut Tx) {
    let Some(panel) = &mut desk.quick else { return };
    if panel.me.is_visible() && desk.focused {
        panel.me.hide();
        return;
    }
    let dial = !std::mem::replace(&mut panel.dialed, true);
    let area = display_under_pointer().map(|[_, _, w, h]| (w, h));
    panel
        .me
        .place(DisplayTarget::UnderPointer, placement(&desk.file, area));
    panel.me.show();
    if dial {
        desk.dial();
    }
}

/// Where the panel opens on a display of `area` points: its default width,
/// `quick-terminal-size` of the height, centred on the chosen edge. An
/// unknown display keeps the window's size.
fn placement(file: &RuneCfg, area: Option<(f32, f32)>) -> OverlayPlacement {
    let bottom = file.quick_terminal_edge == Some(QuickTerminalEdge::Bottom);
    let Some((w, h)) = area else {
        let edge = if bottom {
            ScreenEdge::Bottom
        } else {
            ScreenEdge::Top
        };
        let (margin, inset) = (0.0, 0.0);
        return OverlayPlacement::Edge {
            edge,
            margin,
            align: Align::Center,
            inset,
        };
    };
    let (default, low, high) = HEIGHT;
    let fraction = file.quick_terminal_size.unwrap_or(default).clamp(low, high);
    let width = SIZE.0.min(w);
    let height = (h * fraction).max(MIN_SIZE.1).min(h);
    OverlayPlacement::Rect {
        x: (w - width) / 2.0,
        y: if bottom { h - height } else { 0.0 },
        width,
        height,
    }
}

/// The bounds of the display under the pointer, `[x, y, w, h]` in global
/// points. Rune's placement picks the same display.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn display_under_pointer() -> Option<[f32; 4]> {
    use std::ffi::c_void;
    #[repr(C)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const c_void) -> *mut c_void;
        fn CGEventGetLocation(event: *const c_void) -> Point;
        fn CGGetDisplaysWithPoint(at: Point, max: u32, ids: *mut u32, count: *mut u32) -> i32;
        fn CGDisplayBounds(display: u32) -> Rect;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(object: *const c_void);
    }
    // Safety: CoreGraphics queries with no shared state; the event is a +1
    // CFType released once its location is read.
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return None;
        }
        let at = CGEventGetLocation(event);
        CFRelease(event);
        let (mut display, mut found) = (0, 0);
        if CGGetDisplaysWithPoint(at, 1, &mut display, &mut found) != 0 || found == 0 {
            return None;
        }
        let r = CGDisplayBounds(display);
        Some([r.x as f32, r.y as f32, r.w as f32, r.h as f32])
    }
}

/// Elsewhere the panel keeps its default size at the chosen edge.
#[cfg(not(target_os = "macos"))]
fn display_under_pointer() -> Option<[f32; 4]> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(file: &RuneCfg, area: (f32, f32)) -> [f32; 4] {
        match placement(file, Some(area)) {
            OverlayPlacement::Rect {
                x,
                y,
                width,
                height,
            } => [x, y, width, height],
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_panel_takes_its_share_of_the_display_at_its_edge() {
        let mut file = RuneCfg::default();
        let area = (1920.0, 1080.0);
        assert_eq!(rect(&file, area), [480.0, 0.0, 960.0, 432.0], "0.4, top");
        file.quick_terminal_size = Some(0.75);
        file.quick_terminal_edge = Some(QuickTerminalEdge::Bottom);
        assert_eq!(rect(&file, area), [480.0, 270.0, 960.0, 810.0]);
        file.quick_terminal_size = Some(5.0);
        assert_eq!(rect(&file, area)[3], 1080.0 * 0.9, "held to 0.9");
        file.quick_terminal_size = Some(0.0);
        assert_eq!(rect(&file, area)[3], 1080.0 * 0.2, "held to 0.2");
        // A small display: the panel fits its width, and keeps its least
        // height.
        assert_eq!(rect(&file, (800.0, 600.0)), [0.0, 440.0, 800.0, 160.0]);
    }

    #[test]
    fn an_unknown_display_keeps_the_edge() {
        let file = RuneCfg {
            quick_terminal_edge: Some(QuickTerminalEdge::Bottom),
            ..RuneCfg::default()
        };
        assert!(matches!(
            placement(&file, None),
            OverlayPlacement::Edge {
                edge: ScreenEdge::Bottom,
                ..
            }
        ));
    }
}
