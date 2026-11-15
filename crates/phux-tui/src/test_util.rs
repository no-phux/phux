//! Helpers shared by the crate's tests: key events and rendered overlays.
#![allow(clippy::redundant_pub_crate, reason = "crate-wide test helpers")]

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::render::overlay::RenderOverlay;

/// A press of `key` holding `mods`, producing `text`.
pub(crate) fn key_event(key: PhysicalKey, mods: ModSet, text: Option<&str>) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(ToOwned::to_owned),
        unshifted_codepoint: None,
    }
}

/// A bare press of `key`: no modifiers, no text.
pub(crate) fn press(key: PhysicalKey) -> KeyEvent {
    key_event(key, ModSet::empty(), None)
}

/// An unmodified press of `key` producing `text`.
pub(crate) fn press_text(key: PhysicalKey, text: Option<&str>) -> KeyEvent {
    key_event(key, ModSet::empty(), text)
}

/// A press of `key` holding `mods`, with no text.
pub(crate) fn press_mods(key: PhysicalKey, mods: ModSet) -> KeyEvent {
    key_event(key, mods, None)
}

/// `overlay` rendered into a `cols x rows` buffer, one trimmed line per row.
pub(crate) fn rendered(overlay: &dyn RenderOverlay, cols: u16, rows: u16) -> String {
    let area = Rect::new(0, 0, cols, rows);
    let mut buf = Buffer::empty(area);
    overlay.render(area, &mut buf);
    let mut out = String::new();
    for y in 0..rows {
        let row: String = (0..cols).map(|x| buf[(x, y)].symbol()).collect();
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// `s` with every CSI sequence removed, so content assertions cannot match
/// control bytes.
pub(crate) fn strip_csi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for n in chars.by_ref() {
                if ('@'..='~').contains(&n) {
                    break;
                }
            }
        } else if c != '\x1b' {
            out.push(c);
        }
    }
    out
}

/// A fresh session kernel on the synthesized VT/raw profile.
pub(crate) fn kernel()
-> phux_client_core::session::SessionKernel<phux_client_core::engine::ghostty::GhosttyAdapter> {
    phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    )
}
