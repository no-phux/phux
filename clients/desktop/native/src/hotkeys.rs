//! System-wide hotkeys (Ghostty's `keybind = global:...`), for the quick
//! terminal. Registration goes through Carbon's `RegisterEventHotKey` inside
//! `global-hotkey`, which needs no Accessibility permission; the OS delivers
//! presses on the main thread's event loop into [`presses`], which the app
//! awaits.

use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Every hotkey event this process receives. The handler is process-wide and
/// can be installed only once, so every registration shares this channel.
#[must_use]
pub fn presses() -> async_channel::Receiver<GlobalHotKeyEvent> {
    static PRESSES: OnceLock<async_channel::Receiver<GlobalHotKeyEvent>> = OnceLock::new();
    PRESSES
        .get_or_init(|| {
            let (tx, rx) = async_channel::unbounded();
            GlobalHotKeyEvent::set_event_handler(Some(move |event| {
                let _ = tx.try_send(event);
            }));
            rx
        })
        .clone()
}

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
        let hotkey: HotKey = chord.parse().map_err(|error| format!("{chord}: {error}"))?;
        self.manager
            .register(hotkey)
            .map_err(|error| format!("{chord}: {error}"))?;
        self.chords.insert(hotkey.id(), chord);
        Ok(())
    }

    /// Whether `event` presses a chord this registration holds. Releases are not presses.
    #[must_use]
    pub fn pressed(&self, event: &GlobalHotKeyEvent) -> bool {
        event.state == HotKeyState::Pressed && self.chords.contains_key(&event.id)
    }
}
