//! The quick terminal's global chord: the main window registers it on the
//! panel at launch and again whenever Settings changes it, with no restart.
//! `[rune] quick-terminal` wins over what Settings saved.

use phux_config_core::RuneCfg;
use rune::prefs::Prefs;
use rune::{AppHandle, Shortcut, ShortcutError, ShortcutId};
use rune_kit::ShortcutRecorderState;

use super::super::{Desk, NoticeKind};

/// The chord Settings starts from.
const DEFAULT: &str = "ctrl+backquote";

/// Settings' saved choices.
const ON: &str = "quick_terminal";
const CHORD: &str = "quick_terminal_chord";

/// The main window's side of the quick terminal.
#[derive(Default)]
pub struct Chord {
    /// The panel the chord shows; `None` where there is none (the phone).
    pub(super) panel: Option<AppHandle<Desk>>,
    /// The registration, while one holds.
    id: Option<ShortcutId>,
    /// Whether the quick terminal is on, as chosen or as the file says.
    pub on: bool,
    pub recorder: ShortcutRecorderState,
    /// Who holds the chord the last registration was refused, when known.
    pub owner: Option<String>,
}

impl Chord {
    /// Whether this window has a quick terminal to set up.
    pub fn available(&self) -> bool {
        self.panel.is_some()
    }
}

/// The file's `quick-terminal`: `Some(None)` turns it off (`"none"`, `""`).
fn in_file(file: &RuneCfg) -> Option<Option<&str>> {
    let chord = file.quick_terminal.as_deref()?.trim();
    Some(Some(chord).filter(|chord| !chord.is_empty() && *chord != "none"))
}

/// Whether the quick terminal starts on, and the chord it would use: the
/// file's, else what Settings saved. Off by default: any chord taken
/// globally is taken from every other app.
pub(super) fn at_launch(file: &RuneCfg, prefs: &Prefs) -> (bool, String) {
    match in_file(file) {
        Some(chord) => (chord.is_some(), chord.unwrap_or(DEFAULT).to_owned()),
        None => (
            prefs.get(ON).unwrap_or(false),
            prefs
                .get::<String>(CHORD)
                .filter(|chord| !chord.is_empty())
                .unwrap_or_else(|| DEFAULT.to_owned()),
        ),
    }
}

/// A refusal in words, naming who holds the chord when that's known.
pub fn refusal(error: &ShortcutError, chord: Option<&Shortcut>, owner: Option<&str>) -> String {
    match (error, chord, owner) {
        (ShortcutError::Conflict, Some(chord), Some(owner)) => {
            let sep = if cfg!(target_os = "macos") { "" } else { "+" };
            format!("{} is used by {owner}.", chord.keycaps().join(sep))
        }
        _ => rune_kit::shortcut_error_message(error, chord),
    }
}

impl Desk {
    /// Launch: take the panel, and register the chord when it's on.
    pub fn arm_quick(&mut self, panel: AppHandle<Self>) {
        let (on, chord) = at_launch(&self.file, &self.prefs);
        let shortcut = chord.parse::<Shortcut>();
        self.quick_chord = Chord {
            panel: Some(panel),
            on,
            recorder: ShortcutRecorderState::new(shortcut.clone().ok()),
            ..Chord::default()
        };
        if !on {
            return;
        }
        let result = shortcut
            .map_err(|_| ShortcutError::InvalidAccelerator(chord.clone()))
            .and_then(|shortcut| self.register_quick(Some(&shortcut)));
        if let Err(error) = result {
            self.quick_chord.recorder.mark_unregistered();
            let words = refusal(
                &error,
                self.quick_chord.recorder.shortcut(),
                self.quick_chord.owner.as_deref(),
            );
            self.say(NoticeKind::Error, format!("Quick terminal: {words}"));
        }
    }

    /// Closing the main window keeps the app while the chord can still bring
    /// the panel: on macOS, where the Dock brings the window back.
    pub fn keeps_running(&self) -> bool {
        cfg!(target_os = "macos") && self.quick_chord.id.is_some()
    }

    /// Settings' switch: register the recorded chord, or let it go.
    pub fn toggle_quick_terminal(&mut self) {
        if self.file_managed(self.file.quick_terminal.is_some()) {
            return;
        }
        let on = !self.quick_chord.on;
        let chord = self.quick_chord.recorder.shortcut().cloned();
        let chord = chord.or_else(|| DEFAULT.parse().ok());
        let next = chord.as_ref().filter(|_| on);
        match self.register_quick(next) {
            Ok(()) => {
                self.quick_chord.on = on;
                if on && self.quick_chord.recorder.shortcut().is_none() {
                    self.quick_chord.recorder = ShortcutRecorderState::new(chord);
                }
                let _ = self.prefs.set(ON, &on);
            }
            Err(error) => {
                let owner = self.quick_chord.owner.as_deref();
                let words = refusal(&error, next, owner);
                self.say(NoticeKind::Error, words);
            }
        }
    }

    /// The recorder's change: registered at once, the old chord released
    /// only once the new one holds. Clearing it turns the quick terminal off.
    pub fn record_quick_chord(&mut self, next: Option<&Shortcut>) -> Result<(), ShortcutError> {
        self.register_quick(next)?;
        self.quick_chord.on = next.is_some();
        let _ = self.prefs.set(ON, &next.is_some());
        if let Some(chord) = next {
            let _ = self.prefs.set(CHORD, &chord.to_string());
        }
        Ok(())
    }

    /// Register `next` on the panel before releasing what was registered,
    /// so a refusal keeps the old chord working.
    fn register_quick(&mut self, next: Option<&Shortcut>) -> Result<(), ShortcutError> {
        let quick = &mut self.quick_chord;
        let panel = quick.panel.as_ref().ok_or(ShortcutError::NotRunning)?;
        let new = next.map(|chord| panel.register_shortcut(chord.clone(), super::toggle));
        let new = new.transpose();
        quick.owner = match (&new, next) {
            (Err(ShortcutError::Conflict), Some(chord)) => panel.shortcut_owner(chord),
            _ => None,
        };
        let new = new?;
        if let Some(old) = quick.id
            && let Err(error) = panel.unregister_shortcut(old)
        {
            if let Some(new) = new {
                let _ = panel.unregister_shortcut(new);
            }
            return Err(error);
        }
        quick.id = new;
        Ok(())
    }
}

#[cfg(test)]
impl Desk {
    /// A main window whose panel never ran, so every registration is refused.
    pub(crate) fn quick_fixture(&mut self, on: bool) {
        let panel = rune::App::new(crate::app_spec((320.0, 160.0), (320.0, 160.0)));
        self.quick_chord = Chord {
            panel: Some(panel.handle()),
            on,
            recorder: ShortcutRecorderState::new(DEFAULT.parse().ok()),
            ..Chord::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_wins_and_otherwise_it_is_off_until_chosen() {
        let mut prefs = Prefs::memory();
        let mut file = RuneCfg::default();
        assert_eq!(at_launch(&file, &prefs), (false, DEFAULT.to_owned()));
        let _ = prefs.set(ON, &true);
        let _ = prefs.set(CHORD, &"alt+space".to_owned());
        assert_eq!(at_launch(&file, &prefs), (true, "alt+space".to_owned()));
        for off in ["", "  ", "none"] {
            file.quick_terminal = Some(off.to_owned());
            assert!(!at_launch(&file, &prefs).0, "{off:?} is off");
        }
        file.quick_terminal = Some(" ctrl+shift+t ".to_owned());
        assert_eq!(at_launch(&file, &prefs), (true, "ctrl+shift+t".to_owned()));
    }

    #[test]
    fn a_conflict_names_who_holds_the_chord() {
        let chord: Shortcut = "cmd+space".parse().expect("chord");
        let words = refusal(&ShortcutError::Conflict, Some(&chord), Some("Spotlight"));
        assert!(words.ends_with("is used by Spotlight."), "{words}");
        let words = refusal(&ShortcutError::Conflict, Some(&chord), None);
        assert!(words.ends_with("is already in use."), "{words}");
    }

    #[test]
    fn without_a_panel_nothing_registers_or_keeps_the_app() {
        let mut desk = Desk::settings_test_fixture();
        assert!(!desk.quick_chord.available());
        assert_eq!(
            desk.record_quick_chord(Some(&DEFAULT.parse().expect("chord"))),
            Err(ShortcutError::NotRunning)
        );
        assert!(!desk.keeps_running());
    }
}
