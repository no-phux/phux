//! The phone form (`--phone`): the same runtime in a phone-sized window,
//! with an accessory bar and a drawn keyboard standing in for the system's
//! until Rune has one on a phone (clients/rune/MOBILE.md).

use rune::{Key, KeyEvent, Mods, Phase, Tx};

use super::Desk;
use super::input::Modal;
use crate::keys::{self, Typed};

mod refit;
pub mod spatial;
pub mod touch;

/// Points the arrow pad travels per arrow key it sends.
const PAD_STEP: f32 = 16.0;

/// A modifier the accessory bar holds for the next key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Latch {
    Ctrl,
    Alt,
    Shift,
}

/// A key pressed on the drawn keyboard or the accessory bar.
#[derive(Debug, Clone, PartialEq)]
pub enum Soft {
    Char(char),
    Key(Key),
}

#[derive(Debug, Clone)]
pub struct Phone {
    /// The pruned active group, not the phone's single visible leaf.
    pub tree: Option<phux_client_core::layout::LayoutNode>,
    context: Option<(phux_protocol::ids::SessionKey, [u8; 16])>,
    pub stage: [f32; 2],
    pub travel: [f32; 2],
    pub picker: bool,
    navigation: Option<(phux_protocol::ids::ResourceId, bool)>,
    /// The finger on the terminal, and the coast it left behind.
    touch: Option<touch::Touch>,
    fling: Option<touch::Fling>,
    /// The changed grid waiting for the keyboard or sheet layout to settle.
    refit: Option<refit::Refit>,
    /// What a native host should play next (a haptic).
    pub feedback: Option<touch::Feedback>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The keyboard's own shift: the next letter is a capital.
    pub capital: bool,
    pub keyboard: bool,
    /// The keyboard shows digits and symbols instead of letters.
    pub symbols: bool,
    /// Arrows the pad has sent this drag, across and down.
    pad: (i32, i32),
}

impl Default for Phone {
    fn default() -> Self {
        Self {
            tree: None,
            context: None,
            stage: [0.0; 2],
            travel: [0.0; 2],
            picker: false,
            navigation: None,
            touch: None,
            fling: None,
            refit: None,
            feedback: None,
            ctrl: false,
            alt: false,
            shift: false,
            capital: false,
            keyboard: true,
            symbols: false,
            pad: (0, 0),
        }
    }
}

impl Phone {
    pub fn navigating(&self) -> bool {
        self.navigation.is_some()
    }

    pub(super) fn reconcile(
        &mut self,
        tree: Option<phux_client_core::layout::LayoutNode>,
        context: Option<(phux_protocol::ids::SessionKey, [u8; 16])>,
    ) {
        if self.tree != tree || self.context != context {
            self.reset_navigation();
            self.picker = false;
            self.tree = tree;
            self.context = context;
        }
    }

    pub(super) fn reset_navigation(&mut self) {
        self.navigation = None;
        self.travel = [0.0; 2];
        self.fling = None;
        self.refit = None;
    }

    pub const fn latched(&self, latch: Latch) -> bool {
        match latch {
            Latch::Ctrl => self.ctrl,
            Latch::Alt => self.alt,
            Latch::Shift => self.shift,
        }
    }

    /// Let go of every latch: each holds for one key, as a phone's shift does.
    pub(super) fn release(&mut self) {
        self.ctrl = false;
        self.alt = false;
        self.shift = false;
        self.capital = false;
    }

    /// `soft` as a field types it: a letter takes either shift.
    pub(super) fn for_field(&mut self, soft: Soft) -> Key {
        let key = match soft {
            Soft::Char(c) if self.capital || self.shift => {
                Key::Text(c.to_ascii_uppercase().to_string())
            }
            Soft::Char(c) => Key::Text(c.to_string()),
            Soft::Key(key) => key,
        };
        self.release();
        key
    }

    /// The terminal input for `soft` with what is latched, and the latches
    /// released. A chord with no terminal meaning (⌃ on a symbol) sends
    /// nothing and keeps them, so the lit latch says it is still waiting.
    fn take(&mut self, soft: &Soft) -> Option<Typed> {
        let mods = Mods {
            shift: self.shift,
            ctrl: self.ctrl,
            alt: self.alt,
            ..Mods::default()
        };
        let (key, base) = match soft {
            Soft::Char(c) => {
                let upper = self.capital || (self.shift && !self.ctrl && !self.alt);
                let typed = if upper { c.to_ascii_uppercase() } else { *c };
                (Key::Text(typed.to_string()), Some(c.to_ascii_lowercase()))
            }
            Soft::Key(key) => (key.clone(), None),
        };
        // Alt is Meta here: a phone has no layout characters behind Option.
        let typed = keys::typed(&key, mods, base, None, true)?;
        self.release();
        Some(typed)
    }
}

/// What a drawn key types into while something else has the keyboard.
#[derive(Debug, Clone, Copy)]
enum Field {
    Settings,
    Palette,
    Find,
    Confirm,
    PanePicker,
}

/// How many whole steps `travel` has crossed, toward its sign.
fn steps(travel: f32) -> i32 {
    (travel / PAD_STEP).trunc() as i32
}

impl Desk {
    pub fn with_phone(mut self, phone: bool) -> Self {
        self.phone = phone.then(Phone::default);
        self
    }

    /// A key from the drawn keyboard or the accessory bar, for whatever has
    /// the keyboard: a field or modal first, else the terminal. Delivery is
    /// synchronous, so closing a field cannot redirect a queued key behind it.
    pub fn soft(&mut self, soft: Soft, tx: &mut Tx) {
        self.stop_coast();
        if self.native.is_some() && super::native_input::accessory(self, &soft, tx) {
            return;
        }
        let field = match self.modal() {
            None => return self.soft_to_terminal(&soft),
            Some(Modal::Palette) => Field::Palette,
            Some(Modal::FindField) => Field::Find,
            Some(Modal::Confirm) => Field::Confirm,
            Some(Modal::PhonePicker) => Field::PanePicker,
            Some(Modal::Settings) => Field::Settings,
            Some(Modal::Rename | Modal::Machines | Modal::Inbox) => {
                super::native_input::accessory(self, &soft, tx);
                return;
            }
            Some(_) => return,
        };
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        let event = KeyEvent {
            key: phone.for_field(soft),
            mods: Mods::default(),
        };
        self.soft_to_field(field, &event, tx);
    }

    /// `event` as the field's own keys take it first, then as typing.
    /// Keep this synchronous: `Tx::send_key` routes next frame, when the
    /// field may have closed and its key would reach the terminal instead.
    fn soft_to_field(&mut self, field: Field, event: &KeyEvent, tx: &mut Tx) {
        let taken = match field {
            Field::Settings => {
                self.settings_soft(event, tx);
                return;
            }
            Field::PanePicker => {
                self.phone_picker_key(event);
                return;
            }
            Field::Confirm => {
                self.confirm_key(event);
                return;
            }
            Field::Find => self.find_key(event, tx),
            Field::Palette if event.key == Key::Enter => {
                self.palette_run(None, tx);
                true
            }
            Field::Palette => self.palette_key(event, tx),
        };
        if taken {
            return;
        }
        let edit = match field {
            Field::Find => &mut self.find_query,
            _ => &mut self.palette_query,
        };
        let before = edit.text.clone();
        edit.key(event, None);
        // Only a changed query searches again, as the desktop field does.
        if edit.text == before {
            return;
        }
        match field {
            Field::Find => self.run_search(),
            _ => self.palette_edited(),
        }
    }

    fn soft_to_terminal(&mut self, soft: &Soft) {
        let Some(typed) = self.phone.as_mut().and_then(|phone| phone.take(soft)) else {
            return;
        };
        self.restart_blink();
        self.send_input(|control, terminal| match typed {
            Typed::Text(text) => control.send_text(terminal, &text),
            Typed::Key(atom) => control.send_key(terminal, atom),
        });
    }

    /// One tap for a control chord worth its own key (⌃C), terminal only.
    pub fn soft_chord(&mut self, key: char, tx: &mut Tx) {
        if self.modal().is_some() {
            return;
        }
        if let Some(phone) = self.phone.as_mut() {
            phone.release();
            phone.ctrl = true;
        }
        self.soft(Soft::Char(key), tx);
    }

    pub fn toggle_latch(&mut self, latch: Latch) {
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        let held = match latch {
            Latch::Ctrl => &mut phone.ctrl,
            Latch::Alt => &mut phone.alt,
            Latch::Shift => &mut phone.shift,
        };
        *held = !*held;
    }

    pub fn toggle_capital(&mut self) {
        if let Some(phone) = self.phone.as_mut() {
            phone.capital = !phone.capital;
        }
    }

    pub fn toggle_symbols(&mut self) {
        if let Some(phone) = self.phone.as_mut() {
            phone.symbols = !phone.symbols;
        }
    }

    /// Settings owns its search keyboard without changing the terminal's choice.
    pub fn phone_keyboard_visible(&self) -> bool {
        if self.settings_open {
            self.settings_search_focused()
        } else {
            self.phone.as_ref().is_some_and(|phone| phone.keyboard)
        }
    }

    pub fn toggle_keyboard(&mut self, tx: &mut Tx) {
        if self.settings_open {
            self.blur_settings_search(tx);
            return;
        }
        if let Some(phone) = self.phone.as_mut() {
            phone.keyboard = !phone.keyboard;
        }
    }

    /// Accessory commands follow the same modal policy as native key chords.
    pub fn phone_commands(&mut self, tx: &mut Tx) {
        if self.settings_open {
            self.blur_settings_search(tx);
        } else if let Some(modal) = self.modal() {
            self.modal_command(modal, crate::keys::Shortcut::Palette, tx);
        } else {
            self.shortcut(crate::keys::Shortcut::Palette, tx);
        }
    }

    /// The arrow pad: one arrow per step dragged, along each axis.
    pub fn pad_drag(&mut self, phase: Phase, dx: f32, dy: f32, tx: &mut Tx) {
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        if phase == Phase::Down {
            phone.pad = (0, 0);
        }
        let (across, down) = (steps(dx), steps(dy));
        let (sent_across, sent_down) = phone.pad;
        phone.pad = (across, down);
        let arrows = [
            (across - sent_across, Key::Right, Key::Left),
            (down - sent_down, Key::Down, Key::Up),
        ];
        for (count, forward, back) in arrows {
            let key = if count > 0 { forward } else { back };
            for _ in 0..count.unsigned_abs() {
                self.soft(Soft::Key(key.clone()), tx);
            }
        }
    }
}

#[cfg(test)]
mod runtime;

#[cfg(test)]
mod tests {
    use phux_protocol::input::key::{ModSet, PhysicalKey};

    use rune::prefs::Prefs;
    use rune::{Env, Runtime, TouchPhase};

    use super::*;
    use crate::launch::Resolved;

    fn atom(typed: Option<Typed>) -> (PhysicalKey, ModSet) {
        match typed {
            Some(Typed::Key(event)) => (event.key, event.mods),
            other => panic!("expected a key atom, got {other:?}"),
        }
    }

    #[test]
    fn a_letter_types_and_the_keyboard_shift_capitalizes_once() {
        let mut phone = Phone::default();
        assert_eq!(phone.take(&Soft::Char('a')), Some(Typed::Text("a".into())));
        phone.capital = true;
        assert_eq!(phone.take(&Soft::Char('a')), Some(Typed::Text("A".into())));
        assert_eq!(phone.take(&Soft::Char('a')), Some(Typed::Text("a".into())));
    }

    #[test]
    fn a_latched_ctrl_makes_a_chord_and_lets_go() {
        let mut phone = Phone {
            ctrl: true,
            ..Phone::default()
        };
        let (key, mods) = atom(phone.take(&Soft::Char('c')));
        assert_eq!(key, PhysicalKey::C);
        assert_eq!(mods, ModSet::CTRL);
        assert!(!phone.ctrl);
        assert_eq!(phone.take(&Soft::Char('c')), Some(Typed::Text("c".into())));
    }

    #[test]
    fn alt_is_meta_and_shift_reaches_named_keys() {
        let mut phone = Phone {
            alt: true,
            ..Phone::default()
        };
        assert_eq!(atom(phone.take(&Soft::Char('b'))).1, ModSet::ALT);
        phone.shift = true;
        let (key, mods) = atom(phone.take(&Soft::Key(Key::Tab)));
        assert_eq!((key, mods), (PhysicalKey::Tab, ModSet::SHIFT));
    }

    /// Presses a drawn key the way a finger does.
    fn tap(runtime: &mut Runtime<Desk>, key: &str, at: f64) {
        let (x, y) = runtime.find(key).unwrap_or_else(|| panic!("no {key}"));
        runtime.touch(1, TouchPhase::Started, x, y, at);
        runtime.frame(1.0 / 60.0);
        runtime.touch(1, TouchPhase::Ended, x, y, at + 0.05);
        runtime.frame(1.0 / 60.0);
        // Flush the next frame too: no deferred input may outlive its field.
        runtime.frame(1.0 / 60.0);
    }

    // The keyboard sits outside the palette's scrim; Rune counts what comes
    // after a modal in the tree as part of it. A Rune bump that changes that
    // fails here, not on someone's phone.
    #[test]
    fn the_drawn_keyboard_types_into_the_open_palette() {
        let mut desk = Desk::new(&Resolved::offline("phone")).with_phone(true);
        desk.prefs = Prefs::memory();
        desk.palette_open = true;
        let mut runtime = Runtime::new(desk, crate::ui::view, (393.0, 852.0), Env::new(true, 2.0));
        runtime.on_key = Some(Desk::key);
        runtime.frame(0.0);
        tap(&mut runtime, "phone-latch-Shift", 0.1);
        tap(&mut runtime, "phone-cap-116", 0.3);
        tap(&mut runtime, "phone-cap-104", 0.5);
        assert!(runtime.state.palette_open);
        assert_eq!(runtime.state.palette_query.text, "Th");
    }

    #[test]
    fn the_pad_counts_whole_steps_either_way() {
        assert_eq!(steps(PAD_STEP * 2.5), 2);
        assert_eq!(steps(-PAD_STEP * 1.2), -1);
        assert_eq!(steps(PAD_STEP * 0.9), 0);
    }
}
