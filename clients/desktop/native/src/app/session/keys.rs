//! Key bindings, the native menu bar, and the global quick-terminal hotkeys.

use super::*;

impl Desktop {
    /// Bind `keymap` as GPUI key bindings in the `Desk` context, publish the menus
    /// (which show those bindings), and arm Ghostty's global quick-terminal chords.
    /// An unchanged keymap and chord set does nothing.
    pub fn install_bindings(
        &mut self,
        keymap: Vec<(String, String)>,
        ghostty: &GhosttyConfig,
        ghostty_keys: bool,
        cx: &mut Context<Self>,
    ) {
        let chords = if settings::register_global_hotkeys(Some(ghostty_keys)) {
            settings::quick_terminal_chords(ghostty)
        } else {
            Vec::new()
        };
        if chords != self.hotkey_chords {
            self.hotkey_chords = chords.clone();
            self.register_hotkeys(chords, cx);
        }
        if self.keymap.as_ref() == Some(&keymap) {
            return;
        }
        let strokes: Vec<(String, String)> = keymap
            .iter()
            .filter_map(|(chord, id)| gpui_stroke(chord).map(|stroke| (stroke, id.clone())))
            .collect();
        let mut bindings = Vec::new();
        for old in &self.bound {
            if !strokes.iter().any(|(stroke, _)| stroke == old) {
                bindings.push(KeyBinding::new(old, NoAction, Some(binding_context(old))));
            }
        }
        for (stroke, id) in &strokes {
            bindings.push(KeyBinding::new(
                stroke,
                Run::new(id.clone()),
                Some(binding_context(stroke)),
            ));
        }
        cx.bind_keys(bindings);
        self.bound = strokes.into_iter().map(|(stroke, _)| stroke).collect();
        self.keymap = Some(keymap);
        self.publish_menus(cx);
    }

    /// Menu items name [`Run`] actions by their catalog titles; the platform
    /// menu shows each one's bound chord. The palette holds the rest of the
    /// catalog.
    pub(super) fn publish_menus(&self, cx: &mut App) {
        let item = |id: &str| -> MenuItem {
            if id == MENU_SEPARATOR {
                MenuItem::separator()
            } else {
                MenuItem::action(super::super::actions::title(id), Run::new(id))
            }
        };
        let mut menus = vec![Menu::new("Phux").items([
            item("settings"),
            MenuItem::separator(),
            MenuItem::action("Quit Phux", Quit),
        ])];
        menus.extend(
            MENU_BAR
                .iter()
                .map(|(name, ids)| Menu::new(*name).items(ids.iter().map(|id| item(id)))),
        );
        cx.set_menus(menus);
    }

    /// Replace the global quick-terminal chords. No chords drops the registration.
    pub(super) fn register_hotkeys(&mut self, chords: Vec<String>, cx: &mut Context<Self>) {
        self.hotkeys = None;
        if chords.is_empty() {
            return;
        }
        let mut hotkeys = match GlobalHotkeys::new() {
            Ok(hotkeys) => hotkeys,
            Err(error) => {
                log::error!("global hotkeys unavailable: {error}");
                return;
            }
        };
        let mut registered = false;
        for chord in chords {
            match hotkeys.register(chord.clone()) {
                Ok(()) => registered = true,
                Err(error) => log::error!("global hotkey {chord} unavailable: {error}"),
            }
        }
        if !registered {
            return;
        }
        self.hotkeys = Some(hotkeys);
        if self._hotkey.is_some() {
            return;
        }
        let presses = crate::hotkeys::presses();
        self._hotkey = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = presses.recv().await {
                let alive = this.update(cx, |desktop, cx| {
                    if desktop
                        .hotkeys
                        .as_ref()
                        .is_some_and(|hotkeys| hotkeys.pressed(&event))
                    {
                        desktop.toggle_quick(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }
}
