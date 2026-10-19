//! The quick-terminal window and the home it opens on.

use super::*;

impl Desktop {
    /// Close the quick window if it is open, or open it. A handle whose window
    /// the user already closed counts as closed.
    pub fn toggle_quick(&mut self, cx: &mut Context<Self>) {
        if let Some(opened) = self.quick.take()
            && opened.handle.update(cx, |_, _, _| ()).is_ok()
        {
            opened.desk.update(cx, |desk, cx| desk.prepare_close(cx));
            let _ = opened
                .handle
                .update(cx, |_, window, _| window.remove_window());
            return;
        }
        if let Err(error) = self.resolve_quick_home(cx) {
            if let Some(opened) = self.windows.first() {
                opened.desk.update(cx, |desk, cx| {
                    desk.post(
                        super::super::notifications::Kind::Error,
                        "Could not open quick terminal",
                        Some(error),
                    );
                    cx.notify();
                });
            }
            return;
        }
        let request = self.quick_request(cx);
        let _ = self.open_window(request, true, true, cx);
    }

    pub(super) fn resolve_quick_home(&mut self, cx: &App) -> Result<(), String> {
        let Some(key) = self.home_key else {
            return Ok(());
        };
        for opened in &self.windows {
            if opened.remote.is_some() {
                continue;
            }
            let desk = opened.desk.read(cx);
            let Some(link) = &desk.link else { continue };
            let replica = link.connection.client.organization();
            if replica.is_live() {
                let session = replica.session(&key).ok_or_else(|| "The original home session no longer exists. Open an existing session instead.".to_owned())?;
                self.home_session.clone_from(&session.name);
                return Ok(());
            }
        }
        Err("The home session's identity is unavailable. Reconnect a window before opening the quick terminal.".to_owned())
    }

    /// The quick window restores its own layout and spawns a fresh terminal only
    /// when nothing is saved. Its display is the main window's.
    pub(super) fn quick_request(&self, cx: &App) -> OpenRequest {
        OpenRequest {
            role: WindowRole::Quick,
            session: self.home_session.clone(),
            home_key: self.home_key,
            fresh: true,
            layout_path: self.quick_layout.clone(),
            legacy_layout: self.legacy_quick.clone(),
            stable_layout: !self.explicit_layout,
            main_display: self.main_display(cx),
            ..OpenRequest::default()
        }
    }

    pub(super) fn main_display(&self, cx: &App) -> Option<DisplayPrefs> {
        if let Some(opened) = self.windows.first() {
            return Some(opened.desk.read(cx).prefs.clone());
        }
        let store = FileLayoutStore::new(self.layout.clone(), self.legacy_layout.clone());
        persist::load_layout(&store.read())
            .initial
            .map(|layout| layout.display)
    }
}
