//! Skin, preferences, Ghostty config, paint and layout persistence.

use super::*;

impl Desk {
    /// The skin painting now: the resolved one, or a step toward it while a
    /// shift runs.
    /// A solid window background wears the skin opaque.
    pub(crate) fn skin(&self) -> Skin {
        let skin = self
            .shift
            .map_or(*self.skin_target, |shift| shift.skin(Instant::now()));
        match self.prefs.window_background {
            WindowBackground::Blur => skin,
            WindowBackground::Solid => skin.opaque(),
        }
    }

    /// Linear progress of the running shift, 0 to 1. `None` when settled.
    pub(crate) fn shift_progress(&self) -> Option<f32> {
        self.shift.map(|shift| shift.progress(Instant::now()))
    }

    /// Start a shift when the prefs resolve to a different skin. Without
    /// `animate`, animations off, or under reduced motion, it is instant.
    pub(super) fn sync_skin(&mut self, animate: bool, cx: &mut Context<Self>) {
        let to = resolved_skin(&self.prefs);
        if to.id == self.skin_target.id {
            return;
        }
        let instant = !animate || !self.prefs.animations || cx.reduce_motion();
        self.shift = Some(Shift::new(self.skin(), *to, Instant::now(), instant));
        self.skin_target = to;
        self.set_motion_spans();
        cx.notify();
    }

    pub(super) fn set_motion_spans(&mut self) {
        let motion = self.skin_target.motion;
        let (enter, leave) = if self.prefs.animations {
            (motion.pane_enter(), motion.pane_leave())
        } else {
            (Duration::ZERO, Duration::ZERO)
        };
        self.motion.set_spans(enter, leave);
    }

    /// Called from every render: applies the backdrop for this point of the
    /// shift, keeps frames coming while one runs, and settles Kit when it ends.
    pub(crate) fn drive_skin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_skin(false, cx);
        let now = Instant::now();
        // A solid window is opaque all the way down: the window, its Metal
        // layer and the painted sheet, so the compositor neither blurs nor
        // blends it.
        let backdrop = match (self.shift, self.prefs.window_background) {
            (_, WindowBackground::Solid) => Backdrop::Opaque,
            (Some(shift), WindowBackground::Blur) => shift.backdrop(now),
            (None, WindowBackground::Blur) => self.skin_target.material.backdrop,
        };
        if self.backdrop != Some(backdrop) {
            let appearance = if self.palette().theme.appearance == "light" {
                gpui::WindowAppearance::Light
            } else {
                gpui::WindowAppearance::Dark
            };
            // Installing or clearing native glass reparents the view being
            // drawn, so it waits until this frame is done.
            cx.defer_in(window, move |_, window, _| {
                apply_backdrop(window, backdrop, appearance);
            });
            self.backdrop = Some(backdrop);
        }
        match self.shift {
            Some(shift) if shift.done(now) => {
                self.shift = None;
                self.apply_kit_theme(cx);
            }
            Some(_) => {
                let desk = cx.weak_entity();
                window.on_next_frame(move |_, cx| {
                    let _ = desk.update(cx, |_, cx| cx.notify());
                });
            }
            None => {}
        }
    }

    pub fn ghostty(&self) -> &settings::GhosttyConfig {
        &self.ghostty
    }

    pub(crate) fn keymap(&self) -> &[(String, String)] {
        &self.keymap
    }

    /// The chord `id` shows in menus and tooltips, in glyphs.
    pub fn shown_chord(&self, id: &str) -> Option<String> {
        self.chords.get(id).map(|chord| chord.shown.clone())
    }

    /// The chord `id` shows, as bound (`cmd+t`).
    pub(crate) fn bound_chord(&self, id: &str) -> Option<&str> {
        self.chords.get(id).map(|chord| chord.bound.as_str())
    }

    /// A modal, or the focused pane's find bar, owns the keyboard.
    pub(crate) fn text_field_has_keys(&self) -> bool {
        self.focused_placement()
            .is_some_and(|placement| self.conversation_has_keys(&placement.id))
            || settings::text_field_has_keys(
                !matches!(self.modal, Modal::None),
                self.find.as_ref().map(|find| find.placement_id.as_str()),
                self.focused_placement()
                    .map(|placement| placement.id.as_str()),
            )
    }

    pub(super) fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.modal, Modal::None) {
            self.close_modal(window, cx);
        } else if self.find.is_some() {
            self.close_find(cx);
            self.focus_terminal(window, cx);
            cx.notify();
        }
    }

    pub fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Modal::Path) {
            self.picker = None;
        }
        // Closing the list is what reads it.
        if self.modal == Modal::Notifications {
            self.feed.read_all();
        }
        self.modal = Modal::None;
        self.prepared = Modal::None;
        self.modal_inputs.clear();
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// The open modal changed in place (Go To became Commands) and keeps its controls.
    pub(crate) fn mark_prepared(&mut self) {
        self.prepared = self.modal.clone();
    }

    pub(crate) fn focused_cwd(&self) -> Option<String> {
        self.focused_pane().and_then(|pane| pane.cwd.clone())
    }

    pub(crate) fn workspace_call(&mut self, call: impl FnOnce(&mut Link)) {
        if let Some(link) = self.link.as_mut() {
            call(link);
        }
    }

    /// Change, apply and save display preferences.
    pub(crate) fn update_prefs(
        &mut self,
        change: impl FnOnce(&mut DisplayPrefs),
        cx: &mut Context<Self>,
    ) {
        if self.apply_prefs(change, cx) {
            self.persist();
        }
    }

    /// Change and apply display preferences without writing them, for a
    /// control that moves continuously; [`Self::persist`] saves when it settles.
    pub(crate) fn apply_prefs(
        &mut self,
        change: impl FnOnce(&mut DisplayPrefs),
        cx: &mut Context<Self>,
    ) -> bool {
        let before = self.prefs.clone();
        let axis = self.tab_position();
        change(&mut self.prefs);
        self.prefs = settings::sanitize_display(&self.prefs);
        if self.prefs == before {
            return false;
        }
        self.refresh_tab_placement(axis);
        if self.prefs.sidebar_visible != before.sidebar_visible {
            self.disclose(SIDEBAR_DISCLOSURE.into(), cx);
        }
        if self.prefs.ghostty_keys != before.ghostty_keys {
            self.sync_bindings(cx);
        }
        if self.prefs.theme_id != before.theme_id {
            self.backdrop = None;
            self.apply_kit_theme(cx);
        }
        self.sync_skin(true, cx);
        if self.prefs.animations != before.animations {
            self.set_motion_spans();
        }
        if self.prefs.native_agents != before.native_agents {
            self.snap_conversations(self.prefs.native_agents);
        }
        cx.notify();
        true
    }

    /// Write the layout and display. Tabs are written only once the attached
    /// server's restore has finished, so a half-restored window never replaces a saved layout.
    pub(crate) fn persist(&mut self) {
        if !self.persist_writes {
            return;
        }
        if let Some(next) = self.current_layout() {
            self.last_saved = Some(next);
        } else if let Some(saved) = self.last_saved.as_mut() {
            saved.display = self.prefs.clone();
        } else if self
            .link
            .as_ref()
            .and_then(|link| link.connection.server_id())
            .is_none()
        {
            self.last_saved = Some(persist::save_layout("", &[], &self.prefs, None));
        } else {
            return;
        }
        if let Some(saved) = &self.last_saved {
            self.queue_layout(saved.clone());
        }
    }

    pub(super) fn queue_layout(&mut self, saved: persist::SavedLayout) {
        self.layout_migration_pending = false;
        self.saves.layout(self.store.clone(), saved.clone());
        self.last_saved = Some(saved);
    }

    pub(super) fn flush_saves(&mut self) {
        for error in self.saves.flush() {
            self.post(Kind::Error, "Could not save desktop layout", Some(error));
        }
    }

    /// The layout this window shows, when it owns the attached server's restored tabs.
    pub(super) fn current_layout(&self) -> Option<persist::SavedLayout> {
        let link = self.link.as_ref()?;
        let server = link.connection.server_id()?;
        let owned = link.connection.status == Status::Attached
            && link.workspace.restored()
            && link.workspace.server_id() == Some(server.as_str());
        owned.then(|| {
            persist::save_layout(
                &server,
                &link.workspace.tabs,
                &self.prefs,
                Some(link.workspace.active_id.as_str()),
            )
        })
    }

    /// Save after any workspace change: tabs, splits, ratios, focus, zoom, titles.
    pub(crate) fn persist_if_changed(&mut self) {
        if !self.persist_writes {
            return;
        }
        let Some(next) = self.current_layout() else {
            return;
        };
        if self.layout_migration_pending || self.last_saved.as_ref() != Some(&next) {
            self.queue_layout(next);
        }
    }

    pub(crate) fn reload_ghostty(&mut self, cx: &mut Context<Self>) {
        let sources = settings::read_ghostty_sources();
        self.apply_ghostty_sources(sources.as_deref(), cx);
    }

    /// Adopt freshly read Ghostty files (`None` when there are none): rebind,
    /// drop the cached paint so the Ghostty theme follows, and post a notice
    /// for a missing config or skipped lines. Reload config runs this on what
    /// it reads from disk.
    pub fn apply_ghostty_sources(
        &mut self,
        sources: Option<&[settings::GhosttySource]>,
        cx: &mut Context<Self>,
    ) {
        self.ghostty_found = sources.is_some();
        self.ghostty = settings::parse_ghostty_sources(sources.unwrap_or_default());
        self.sync_bindings(cx);
        self.apply_kit_theme(cx);
        if sources.is_none() {
            self.post(Kind::Refusal, "No Ghostty config found", None);
        }
        if let Some((title, body)) = settings::skipped_lines_notice(&self.ghostty) {
            self.post(Kind::Refusal, &title, Some(body));
        }
        cx.notify();
    }

    /// Install a Ghostty config as if `reload-config` read it, with its binds on.
    #[cfg(test)]
    pub(crate) fn install_ghostty_for_test(&mut self, text: &str, cx: &mut Context<Self>) {
        self.ghostty = settings::parse_ghostty(text);
        self.prefs.ghostty_keys = true;
        self.sync_bindings(cx);
    }

    pub(crate) fn import_ghostty(&mut self, cx: &mut Context<Self>) {
        let next = settings::ghostty_prefs(&self.prefs, &self.ghostty);
        self.update_prefs(|prefs| *prefs = next, cx);
        self.apply_kit_theme(cx);
    }

    pub fn terminal_settings(
        &self,
        placement: &Placement,
        focused: bool,
    ) -> Option<TerminalSettings> {
        placement.terminal_id.as_ref()?;
        let mut settings = self.paint().terminal.clone();
        settings.focused = focused;
        settings.size_owner = self
            .link
            .as_ref()
            .is_some_and(|link| link.workspace.size_owner(placement));
        Some(settings)
    }

    /// Paint values for the current preferences and skin, rebuilt only after
    /// they change.
    pub(crate) fn paint(&self) -> Rc<Paint> {
        let skin = self.skin();
        let mut cached = self.paint.borrow_mut();
        match cached.as_ref() {
            Some(paint) if paint.prefs == self.prefs && paint.skin == skin => paint.clone(),
            _ => {
                let paint = Rc::new(Paint::new(&self.prefs, &skin, &self.ghostty, &self.keymap));
                *cached = Some(paint.clone());
                paint
            }
        }
    }

    /// The skin resolved against the theme, for this frame.
    pub(crate) fn look(&self) -> Rc<Look> {
        self.paint().look.clone()
    }

    pub fn palette(&self) -> crate::settings::theme::Palette {
        self.paint().palette.clone()
    }
}
