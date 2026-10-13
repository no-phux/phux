//! System-wide hotkeys (Ghostty's `keybind = global:...`), for the quick
//! terminal. Registration goes through Carbon's `RegisterEventHotKey` inside
//! `global-hotkey`, which needs no Accessibility permission; the OS delivers
//! presses on the main thread's event loop, and the app drains them.

use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use std::collections::BTreeMap;

/// Hotkeys registered by this process. Dropping it unregisters them.
pub struct GlobalHotkeys {
    manager: GlobalHotKeyManager,
    chords: BTreeMap<u32, String>,
}

impl GlobalHotkeys {
    pub fn new() -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new().map_err(|error| error.to_string())?;
        Ok(Self {
            manager,
            chords: BTreeMap::new(),
        })
    }

    /// Register a chord in `cmd+ctrl+alt+shift+key` form. Fails when another
    /// app (Ghostty, for one) already holds it.
    pub fn register(&mut self, chord: String) -> Result<(), String> {
        let hotkey: HotKey = chord
            .parse()
            .map_err(|error| format!("{chord}: {error}"))?;
        self.manager
            .register(hotkey)
            .map_err(|error| format!("{chord}: {error}"))?;
        self.chords.insert(hotkey.id(), chord);
        Ok(())
    }

    /// Chords pressed since the last call, oldest first. Releases are dropped.
    pub fn take_pressed(&self) -> Vec<String> {
        let mut pressed = Vec::new();
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state == HotKeyState::Pressed
                && let Some(chord) = self.chords.get(&event.id)
            {
                pressed.push(chord.clone());
            }
        }
        pressed
    }
}
