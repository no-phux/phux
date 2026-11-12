//! The quick terminal: a panel a global chord drops over whatever app is in
//! front, on the current Space, holding one terminal of its own session.
//!
//! It is a second window of this app with its own [`Desk`], always on this
//! computer's server. Showing it never activates the app or raises the main
//! window, so a window manager or Stage Manager doesn't switch away; it hides
//! again when it loses the keyboard. Its session is an ordinary one, so what
//! runs there outlives the panel and can be attached from anywhere.

use rune::{Align, DisplayTarget, OverlayPlacement, ScreenEdge, Tx, WindowOptions, WindowSpec};

use super::Desk;
use crate::launch::Resolved;

/// The session the panel shows, made again if its last terminal exits.
pub const SESSION: &str = "quick";

const SIZE: (f32, f32) = (960.0, 440.0);
const MIN_SIZE: (f32, f32) = (320.0, 160.0);

/// `NSWindowCollectionBehavior`: CanJoinAllSpaces | FullScreenAuxiliary, so
/// it opens over the Space, or the full-screen app, in front.
const ALL_SPACES: usize = 1 | 1 << 8;

/// The chord in the config file, read once at launch.
pub fn configured() -> Option<String> {
    chord(&super::settings::load_file().0).map(str::to_owned)
}

/// The chord set as `[rune] quick-terminal`; unset or `"none"` is off. Off
/// by default: any chord taken globally is taken from every other app.
fn chord(file: &phux_config_core::RuneCfg) -> Option<&str> {
    file.quick_terminal
        .as_deref()
        .map(str::trim)
        .filter(|chord| !chord.is_empty() && *chord != "none")
}

impl Desk {
    /// The panel's window state: this computer's server and the quick session.
    fn quick(resolved: &Resolved, me: rune::AppHandle<Self>) -> Self {
        let mut desk = Self::open(resolved, None);
        desk.launch.session = Some(SESSION.to_owned());
        desk.quick = Some(me);
        // It opens hidden, so nothing of it has the keyboard yet.
        desk.focused = false;
        desk
    }
}

/// Add the panel to `app`, shown and hidden by `chord`.
pub fn add(app: &mut rune::App<Desk>, chord: &str, resolved: Resolved) {
    let panel = rune::App::new(crate::app_spec(SIZE, MIN_SIZE)).windowless();
    let me = panel.handle();
    let panel = panel
        .init(move || Desk::quick(&resolved, me))
        .global_shortcut(chord, toggle)
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
    app.window(WindowSpec::from_app("quick", panel).configured_options(options));
}

/// The chord: hide the panel while it has the keyboard, else bring it down
/// from the top of the display under the pointer.
fn toggle(desk: &mut Desk, _: &mut Tx) {
    let Some(me) = &desk.quick else { return };
    if me.is_visible() && desk.focused {
        me.hide();
        return;
    }
    me.place(
        DisplayTarget::UnderPointer,
        OverlayPlacement::Edge {
            edge: ScreenEdge::Top,
            margin: 0.0,
            align: Align::Center,
            inset: 0.0,
        },
    );
    me.show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chord_is_off_unless_named() {
        let mut file = phux_config_core::RuneCfg::default();
        assert_eq!(chord(&file), None, "off by default");
        for off in ["", "  ", "none"] {
            file.quick_terminal = Some(off.to_owned());
            assert_eq!(chord(&file), None, "{off:?} is off");
        }
        file.quick_terminal = Some(" ctrl+backquote ".to_owned());
        assert_eq!(chord(&file), Some("ctrl+backquote"));
    }
}
