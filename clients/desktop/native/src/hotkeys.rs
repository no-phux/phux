//! System-wide hotkeys (Ghostty's `keybind = global:...`), for the quick
//! terminal. Registration goes through Carbon's `RegisterEventHotKey` inside
//! `global-hotkey`, which needs no Accessibility permission; the OS delivers
//! presses on the main thread's event loop, and the shell drains them.

use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use napi::bindgen_prelude::Result;
use napi_derive::napi;
use std::collections::BTreeMap;

/// Hotkeys registered by this process. Dropping it unregisters them.
#[napi]
pub struct GlobalHotkeys {
    manager: GlobalHotKeyManager,
    chords: BTreeMap<u32, String>,
}

#[napi]
impl GlobalHotkeys {
    #[napi(constructor)]
    pub fn new() -> Result<Self> {
        let manager = GlobalHotKeyManager::new()
            .map_err(|error| napi::Error::from_reason(error.to_string()))?;
        Ok(Self {
            manager,
            chords: BTreeMap::new(),
        })
    }

    /// Register a chord in the shell's `cmd+ctrl+alt+shift+key` form. Fails
    /// when another app (Ghostty, for one) already holds it.
    #[napi]
    pub fn register(&mut self, chord: String) -> Result<()> {
        let hotkey: HotKey = chord
            .parse()
            .map_err(|error| napi::Error::from_reason(format!("{chord}: {error}")))?;
        self.manager
            .register(hotkey)
            .map_err(|error| napi::Error::from_reason(format!("{chord}: {error}")))?;
        self.chords.insert(hotkey.id(), chord);
        Ok(())
    }

    /// Chords pressed since the last call, oldest first. Releases are dropped.
    #[napi]
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
