//! The [`ratcn`] bridge: overlays build on ratcn's components and interaction
//! runtime, fed from phux's own theme and protocol input.
//!
//! - [`theme`] maps the solved phux [`Theme`] onto ratcn's roles, so ratcn
//!   components paint in the phux design language at the terminal's depth.
//! - [`key`] / [`mouse`] translate protocol input atoms (ADR-0006/0008) into
//!   ratcn's backend-neutral [`Event`]; ratcn never reads the terminal.
//! - [`Surface`] owns one retained [`Ratcn`] runtime for an overlay. Painting
//!   goes through `&self` (the [`RenderOverlay`](super::overlay::RenderOverlay)
//!   contract), and routing an event before the first paint declares the
//!   surface offscreen first, since ratcn ignores input until it has rendered.

use std::cell::{Cell, RefCell};

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratcn::runtime::{
    self as rt, DeclareCtx, Event, EventResult, KeyCode, Modifiers, MouseKind, Ratcn,
    ScrollDirection,
};

use crate::render::Theme;

/// The area an overlay is declared in before it has ever painted: the
/// classic terminal size, so offscreen routing lays components out as a
/// real frame would.
const OFFSCREEN: Rect = Rect::new(0, 0, 80, 24);

/// The phux [`Theme`] expressed as ratcn roles.
///
/// Overlays float on [`Theme::surface`], so every ratcn surface role rides
/// it; the selected-row pair carries ratcn's `primary` (default buttons,
/// selected rows), focus rides the accent like every other phux focus cue.
#[must_use]
pub const fn theme(t: &Theme) -> ratcn::Theme {
    let mut out = ratcn::Theme::default_dark();
    out.name = "phux";
    out.foreground = t.text;
    out.muted_foreground = t.dim;
    out.background = t.surface;
    out.surface = t.surface;
    out.field = t.surface;
    out.primary = t.selection_bg;
    out.primary_foreground = t.selection_fg;
    out.secondary = t.surface;
    out.secondary_foreground = t.dim;
    out.accent = t.accent;
    out.destructive = t.error;
    out.destructive_foreground = t.surface;
    out.warning = t.attention;
    out.border = t.border;
    out.ring = t.accent;
    out.cursor = t.text;
    out
}

/// A protocol key press as a ratcn [`Event`], or `None` for a release, an
/// IME composition step, or a key ratcn has no name for.
#[must_use]
pub fn key(key: &KeyEvent) -> Option<Event> {
    if key.action == KeyAction::Release || key.composing {
        return None;
    }
    let modifiers = Modifiers {
        ctrl: key.mods.contains(ModSet::CTRL),
        alt: key.mods.contains(ModSet::ALT),
        shift: key.mods.contains(ModSet::SHIFT),
    };
    let code = key_code(key, modifiers.shift)?;
    Some(Event::Key(rt::KeyEvent { code, modifiers }))
}

fn key_code(key: &KeyEvent, shift: bool) -> Option<KeyCode> {
    let code = match key.key {
        PhysicalKey::Enter | PhysicalKey::NumpadEnter => KeyCode::Enter,
        PhysicalKey::Escape => KeyCode::Esc,
        PhysicalKey::Tab if shift => KeyCode::BackTab,
        PhysicalKey::Tab => KeyCode::Tab,
        PhysicalKey::Backspace | PhysicalKey::NumpadBackspace => KeyCode::Backspace,
        PhysicalKey::Delete | PhysicalKey::NumpadDelete => KeyCode::Delete,
        PhysicalKey::ArrowLeft | PhysicalKey::NumpadLeft => KeyCode::Left,
        PhysicalKey::ArrowRight | PhysicalKey::NumpadRight => KeyCode::Right,
        PhysicalKey::ArrowUp | PhysicalKey::NumpadUp => KeyCode::Up,
        PhysicalKey::ArrowDown | PhysicalKey::NumpadDown => KeyCode::Down,
        PhysicalKey::Home | PhysicalKey::NumpadHome => KeyCode::Home,
        PhysicalKey::End | PhysicalKey::NumpadEnd => KeyCode::End,
        PhysicalKey::PageUp | PhysicalKey::NumpadPageUp => KeyCode::PageUp,
        PhysicalKey::PageDown | PhysicalKey::NumpadPageDown => KeyCode::PageDown,
        other => return function_key(other).or_else(|| char_key(key)),
    };
    Some(code)
}

/// `F1`..=`F25` as `F(n)`.
fn function_key(key: PhysicalKey) -> Option<KeyCode> {
    let n = (key as u32).checked_sub(PhysicalKey::F1 as u32)?;
    let last = PhysicalKey::F25 as u32 - PhysicalKey::F1 as u32;
    (n <= last).then(|| KeyCode::F(u8::try_from(n + 1).unwrap_or(u8::MAX)))
}

/// The character a key types: its layout text when it carries exactly one
/// character, else (Ctrl/Alt chords carry none) the letter, digit, or space
/// the physical key names.
fn char_key(key: &KeyEvent) -> Option<KeyCode> {
    if let Some(text) = key.text.as_deref() {
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Some(KeyCode::Char(c));
        }
        return None;
    }
    let raw = key.key as u32;
    let letter = raw.checked_sub(PhysicalKey::A as u32).filter(|n| *n < 26);
    let digit = raw
        .checked_sub(PhysicalKey::Digit0 as u32)
        .filter(|n| *n < 10);
    let c = match (letter, digit, key.key) {
        (Some(n), _, _) => char::from(b'a' + u8::try_from(n).ok()?),
        (_, Some(n), _) => char::from(b'0' + u8::try_from(n).ok()?),
        (_, _, PhysicalKey::Space) => ' ',
        _ => return None,
    };
    Some(KeyCode::Char(c))
}

/// A protocol mouse atom (cell coordinates carried in the pixel fields, as
/// the attach driver hands overlays) as a ratcn [`Event`], or `None` for a
/// button ratcn does not track.
#[must_use]
pub fn mouse(mouse: &MouseEvent) -> Option<Event> {
    let kind = match (mouse.action, mouse.button) {
        (MouseAction::Press, MouseButton::Four) => MouseKind::Scroll(ScrollDirection::Up),
        (MouseAction::Press, MouseButton::Five) => MouseKind::Scroll(ScrollDirection::Down),
        (MouseAction::Press, MouseButton::Six) => MouseKind::Scroll(ScrollDirection::Left),
        (MouseAction::Press, MouseButton::Seven) => MouseKind::Scroll(ScrollDirection::Right),
        (MouseAction::Press, button) => MouseKind::Down(button_of(button)?),
        (MouseAction::Release, button) => {
            MouseKind::Up(button_of(button).unwrap_or(rt::MouseButton::Left))
        }
        (MouseAction::Motion, MouseButton::Unknown) => MouseKind::Moved,
        (MouseAction::Motion, button) => MouseKind::Drag(button_of(button)?),
    };
    Some(Event::Mouse(rt::MouseEvent {
        kind,
        column: cell(mouse.x),
        row: cell(mouse.y),
        modifiers: Modifiers {
            ctrl: mouse.mods.contains(ModSet::CTRL),
            alt: mouse.mods.contains(ModSet::ALT),
            shift: mouse.mods.contains(ModSet::SHIFT),
        },
    }))
}

const fn button_of(button: MouseButton) -> Option<rt::MouseButton> {
    match button {
        MouseButton::Left => Some(rt::MouseButton::Left),
        MouseButton::Right => Some(rt::MouseButton::Right),
        MouseButton::Middle => Some(rt::MouseButton::Middle),
        _ => None,
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=u16::MAX first"
)]
fn cell(p: f64) -> u16 {
    p.max(0.0).min(f64::from(u16::MAX)) as u16
}

/// One overlay's retained ratcn runtime.
///
/// The overlay keeps its model `S` itself and declares its components from
/// it each frame; the runtime keeps only what ratcn retains between frames
/// (the declared surface, pointer gestures, hover).
pub struct Surface<S, M> {
    runtime: RefCell<Ratcn<S, M>>,
    /// The area of the last paint, so an offscreen declaration before the
    /// next one lays out the same geometry the user is looking at.
    area: Cell<Option<Rect>>,
}

impl<S, M> std::fmt::Debug for Surface<S, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Surface")
            .field("area", &self.area.get())
            .finish_non_exhaustive()
    }
}

impl<S: 'static, M: 'static> Surface<S, M> {
    /// Wrap a configured runtime (focus/modal bindings already attached).
    #[must_use]
    pub const fn new(runtime: Ratcn<S, M>) -> Self {
        Self {
            runtime: RefCell::new(runtime),
            area: Cell::new(None),
        }
    }

    /// Declare and paint this frame's components into `buf` over `area`.
    pub fn paint(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &S,
        theme: &ratcn::Theme,
        declare: impl FnMut(&mut DeclareCtx<'_, S, M>),
    ) {
        self.area.set(Some(area));
        self.runtime
            .borrow_mut()
            .render_into(buf, area, state, theme, declare);
    }

    /// Route `event` through the surface last declared. Before any paint the
    /// surface is first declared offscreen with `declare`, so input that
    /// arrives ahead of the first frame (or in a headless test) still lands.
    pub fn route(
        &mut self,
        event: Event,
        state: &S,
        theme: &ratcn::Theme,
        declare: impl FnMut(&mut DeclareCtx<'_, S, M>),
    ) -> EventResult<M> {
        if self.area.get().is_none() {
            let area = OFFSCREEN;
            let mut scratch = Buffer::empty(area);
            self.paint(&mut scratch, area, state, theme, declare);
        }
        self.runtime.get_mut().handle_event(event, state)
    }

    /// Forget the painted geometry (after a resize), so the next routed
    /// event re-declares before it hit-tests.
    pub fn invalidate(&self) {
        self.area.set(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{key_event, press, press_text};

    #[expect(clippy::needless_pass_by_value, reason = "reads as a call wrapper")]
    fn code(event: Option<Event>) -> Option<KeyCode> {
        match event {
            Some(Event::Key(k)) => Some(k.code),
            _ => None,
        }
    }

    #[test]
    fn named_keys_map_to_ratcn_codes() {
        assert_eq!(code(key(&press(PhysicalKey::Enter))), Some(KeyCode::Enter));
        assert_eq!(code(key(&press(PhysicalKey::Escape))), Some(KeyCode::Esc));
        assert_eq!(code(key(&press(PhysicalKey::F1))), Some(KeyCode::F(1)));
        assert_eq!(code(key(&press(PhysicalKey::F12))), Some(KeyCode::F(12)));
        let back = key_event(PhysicalKey::Tab, ModSet::SHIFT, None);
        assert_eq!(code(key(&back)), Some(KeyCode::BackTab));
    }

    #[test]
    fn text_wins_and_chords_fall_back_to_the_physical_key() {
        let typed = press_text(PhysicalKey::A, Some("Å"));
        assert_eq!(code(key(&typed)), Some(KeyCode::Char('Å')));
        let chord = key_event(PhysicalKey::W, ModSet::CTRL, None);
        let Some(Event::Key(k)) = key(&chord) else {
            panic!("ctrl+w converts");
        };
        assert_eq!((k.code, k.modifiers.ctrl), (KeyCode::Char('w'), true));
        assert_eq!(
            code(key(&press(PhysicalKey::Digit7))),
            Some(KeyCode::Char('7'))
        );
        assert_eq!(
            code(key(&press(PhysicalKey::Space))),
            Some(KeyCode::Char(' '))
        );
    }

    #[test]
    fn releases_and_compositions_do_not_convert() {
        let mut release = press(PhysicalKey::Enter);
        release.action = KeyAction::Release;
        assert_eq!(key(&release), None);
        let mut composing = press_text(PhysicalKey::A, Some("a"));
        composing.composing = true;
        assert_eq!(key(&composing), None);
    }

    #[test]
    fn mouse_atoms_map_to_ratcn_kinds_at_their_cell() {
        let at = |action, button| MouseEvent {
            action,
            button,
            mods: ModSet::empty(),
            x: 7.0,
            y: 3.0,
        };
        let kind = |e: Option<Event>| match e {
            Some(Event::Mouse(m)) => Some((m.kind, m.column, m.row)),
            _ => None,
        };
        assert_eq!(
            kind(mouse(&at(MouseAction::Press, MouseButton::Left))),
            Some((MouseKind::Down(rt::MouseButton::Left), 7, 3))
        );
        assert_eq!(
            kind(mouse(&at(MouseAction::Press, MouseButton::Five))),
            Some((MouseKind::Scroll(ScrollDirection::Down), 7, 3))
        );
        assert_eq!(
            kind(mouse(&at(MouseAction::Motion, MouseButton::Unknown))),
            Some((MouseKind::Moved, 7, 3))
        );
        assert_eq!(mouse(&at(MouseAction::Press, MouseButton::Eight)), None);
    }
}
