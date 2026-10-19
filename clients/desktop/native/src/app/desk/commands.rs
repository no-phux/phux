//! The command path shared by keys, menus and controls.

use super::*;

impl Desk {
    pub fn execute(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_field_has_keys() && settings::yields_to_text_field(id) {
            return;
        }
        if self.modal != Modal::None {
            // A modal's own command closes it.
            let toggle = listed_modal(id).as_ref() == Some(&self.modal);
            self.close_modal(window, cx);
            if toggle {
                return;
            }
        }
        if let Some(classified) = settings::classify_command(id) {
            use settings::BoundAction::*;
            match classified {
                Ignore => {}
                Send(text) => self.send_text(&text),
                WriteFile { region, mode } => self.write_file(&region, &mode, cx),
                FontSize(size) => self.update_prefs(|prefs| prefs.font_size = size, cx),
                ScrollLines(rows) => self.scroll(Scroll::Delta(rows as i64), cx),
                PromptJump(prompts) => self.scroll(Scroll::Prompt(prompts as i64), cx),
            }
            return;
        }
        if !self.run_organization(id, window, cx) {
            self.run_catalog(id, window, cx);
        }
    }

    pub(super) fn run_catalog(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            // A pending approval is server state, not a message: it leaves
            // the Inbox when it is decided, expires, or is withdrawn.
            "clear-notifications" => self.feed.retain(|entry| entry.kind == Kind::Approval),
            "new" | "new-home" => {
                let home = std::env::var("HOME").ok().filter(|_| id == "new-home");
                self.workspace_call(|link| {
                    link.workspace
                        .new_terminal(&link.connection, home.as_deref())
                });
            }
            "new-session" => {
                self.modal = Modal::Session {
                    create: true,
                    current: String::new(),
                    target: None,
                }
            }
            "rename-session" => self.open_rename_session(),
            "kill-session" => self.ask_kill_session(),
            "folder" => self.open_folder(window, cx),
            "native-agent" => self.open_native_launch(window, cx),
            "insert-path" => self.open_path_picker(),
            "view" => self.workspace_call(|link| link.workspace.duplicate_view(&link.connection)),
            "close" => self.fold_pane(window, cx),
            "close-tab" => {
                if let Some(id) = self.active_tab_id() {
                    self.workspace_call(|link| link.workspace.close_tab(&link.connection, &id));
                }
            }
            "zoom" => self.workspace_call(|link| link.workspace.toggle_zoom()),
            "equalize" => self.workspace_call(|link| link.workspace.equalize_tab()),
            "pane-next" => self.workspace_call(|link| link.workspace.cycle_pane(1)),
            "pane-prev" => self.workspace_call(|link| link.workspace.cycle_pane(-1)),
            "tab-next" => self.workspace_call(|link| link.workspace.step_tab(1)),
            "tab-prev" => self.workspace_call(|link| link.workspace.step_tab(-1)),
            "tab-move-left" => self.move_active_tab(-1),
            "tab-move-right" => self.move_active_tab(1),
            "rename" => self.rename_active(),
            "window" => self.move_to_window(cx),
            "window-next" => self.move_to_next_window(cx),
            "find" => self.open_find(window, cx),
            "find-next" => self.step_find(1, window, cx),
            "find-prev" => self.step_find(-1, window, cx),
            "find-selection" => self.find_selection(window, cx),
            "find-close" => self.close_find(cx),
            "follow" | "scroll-bottom" => self.follow(cx),
            "paste-cwd" => self.paste_cwd(),
            "terminate" => self.ask_terminate(),
            "attention" => self.next_attention(),
            "native-agents" => self.toggle_native_agents(window, cx),
            "conversation" => self.toggle_focused_conversation(window, cx),
            "changes" => self.toggle_changes(cx),
            "sidebar" => {
                self.update_prefs(|prefs| prefs.sidebar_visible = !prefs.sidebar_visible, cx)
            }
            "plain-terminal" => self.update_prefs(|prefs| prefs.plain = !prefs.plain, cx),
            "tabs-top" => self.update_prefs(|prefs| prefs.tab_position = TabPosition::Top, cx),
            "tabs-bottom" => {
                self.update_prefs(|prefs| prefs.tab_position = TabPosition::Bottom, cx)
            }
            "tabs-side" => self.update_prefs(
                |prefs| {
                    prefs.tab_position = TabPosition::Side;
                    prefs.sidebar_visible = true;
                },
                cx,
            ),
            "group-projects" => self.group_sidebar(SidebarGrouping::Project, cx),
            "group-machines" => self.group_sidebar(SidebarGrouping::Machine, cx),
            "group-none" => self.group_sidebar(SidebarGrouping::None, cx),
            "main-menu" => self.open_main_menu(window, cx),
            "font-up" => self.update_prefs(|prefs| prefs.font_size += 1.0, cx),
            "font-down" => self.update_prefs(|prefs| prefs.font_size -= 1.0, cx),
            "font-reset" => self.update_prefs(
                |prefs| prefs.font_size = DisplayPrefs::default().font_size,
                cx,
            ),
            "scroll-page-up" => self.scroll_page(-1, cx),
            "scroll-page-down" => self.scroll_page(1, cx),
            "scroll-top" => self.scroll(Scroll::Top, cx),
            "clear" => self.send_text("\u{000c}"),
            "select-all" => self.select_all(cx),
            "copy" => self.host_action(HostAction::Copy, cx),
            "paste" => self.host_action(HostAction::Paste, cx),
            "paste-selection" => self.paste_selection(),
            "prompt-prev" => self.scroll(Scroll::Prompt(-1), cx),
            "prompt-next" => self.scroll(Scroll::Prompt(1), cx),
            // The quick window's own desk may be the one asking.
            "quick-terminal" => self.defer_desktop(cx, |desktop, cx| desktop.toggle_quick(cx)),
            "new-window" => self.new_window(window, cx),
            "minimize" => window.minimize_window(),
            "fullscreen" => window.toggle_fullscreen(),
            "maximize" => window.zoom_window(),
            "reload-config" => self.reload_ghostty(cx),
            "import-ghostty" => self.import_ghostty(cx),
            "reconnect" => self.reconnect(cx),
            "machines" => {
                self.show_settings_page(super::super::settings_panel::Page::Connections, window, cx)
            }
            "add-host" => self.modal = Modal::AddHost,
            "copy-diagnostics" => self.copy_diagnostics(cx),
            // Opening/closing windows must wait until this desk's window lease ends.
            "performance-monitor" => self.defer_desktop(cx, |desktop, cx| {
                if let Err(error) = desktop.open_performance(cx) {
                    log::error!("could not open performance monitor: {error:#}");
                }
            }),
            "performance-monitor-stop" => {
                self.defer_desktop(cx, |desktop, cx| desktop.stop_performance(cx))
            }
            other => self.run_patterned(other),
        }
        self.after_command(window, cx);
    }

    /// Commands named by pattern: a modal, a direction, or a tab number.
    pub(super) fn run_patterned(&mut self, id: &str) {
        if let Some(modal) = listed_modal(id) {
            self.modal = modal;
            return;
        }
        match directional(id) {
            Some(("split", direction)) => {
                let axis = match direction {
                    Direction::Left | Direction::Right => Axis::Row,
                    Direction::Up | Direction::Down => Axis::Column,
                };
                let before = matches!(direction, Direction::Left | Direction::Up);
                self.workspace_call(|link| link.workspace.split(&link.connection, axis, before));
            }
            Some(("pane", direction)) => {
                self.workspace_call(|link| link.workspace.focus_direction(direction))
            }
            Some(("resize", direction)) => {
                self.workspace_call(|link| link.workspace.nudge(direction))
            }
            _ => {
                if let Some(index) = id
                    .strip_prefix("tab-")
                    .and_then(|index| index.parse::<isize>().ok())
                {
                    let index = if index == 9 { -1 } else { index - 1 };
                    self.workspace_call(|link| link.workspace.select_index(index));
                }
            }
        }
    }

    /// Run on the desktop once this update ends: a desk is often reached
    /// from inside a `Desktop` update.
    pub(super) fn defer_desktop(
        &self,
        cx: &mut Context<Self>,
        call: impl FnOnce(&mut Desktop, &mut Context<Desktop>) + 'static,
    ) {
        let desktop = self.desktop.clone();
        cx.defer(move |cx| desktop.update(cx, call));
    }

    /// Every command and pointer action ends here: modal controls take the
    /// keyboard, or the terminal gets it back, and a changed layout is saved.
    pub(crate) fn after_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.take_notices();
        self.retain_panes();
        if self.modal != self.prepared {
            // Build and focus the open modal's controls: a fresh query or prefilled field each time.
            self.prepared = self.modal.clone();
            self.modal_inputs.clear();
            super::super::overlays::prepare(self, window, cx);
        }
        self.focus_terminal(window, cx);
        self.sync_title(window);
        self.persist_if_changed();
        self.follow_changes(cx);
        cx.notify();
    }

    /// Keys the window owns outside its key bindings. Escape closes the modal,
    /// else the find bar that holds the keys, and otherwise reaches the terminal.
    pub fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        let action: Option<fn(&mut Self, &mut Window, &mut Context<Self>)> = match key {
            _ if modifiers.modified() => None,
            // The conversation took the escape already.
            "escape" if self.conversation_escape(cx) => Some(|_, _, _| {}),
            "escape" if self.text_field_has_keys() => Some(Self::escape),
            "enter" => match self.modal {
                Modal::Terminate { .. } => Some(Self::confirm_terminate),
                Modal::KillSession { .. } => Some(Self::confirm_kill_session),
                Modal::RemoveHost { .. } => Some(Self::confirm_remove_host),
                Modal::Paste { .. } => Some(Self::confirm_paste),
                _ => None,
            },
            "right" if self.modal == Modal::Path => Some(Self::open_selected_directory),
            _ => None,
        };
        if let Some(action) = action {
            action(self, window, cx);
            cx.stop_propagation();
            return;
        }
        let chord = shell_chord(
            modifiers.platform,
            modifiers.control,
            modifiers.alt,
            modifiers.shift,
            key,
            event.is_held,
        );
        if chord.is_empty() {
            return;
        }
        let settings_chord = settings::key_chord(&settings::KeyChord {
            key: key.to_owned(),
            held: event.is_held,
            cmd: modifiers.platform,
            alt: modifiers.alt,
            ctrl: modifiers.control,
            shift: modifiers.shift,
        });
        let chord = if settings_chord.is_empty() {
            chord
        } else {
            settings_chord
        };
        // Chords GPUI can name are key bindings already; only the rest arrive here.
        if gpui_stroke(&chord).is_some() {
            return;
        }
        if let Some((_, id)) = self.keymap.iter().find(|(bound, _)| *bound == chord) {
            window.dispatch_action(Box::new(Run::new(id.clone())), cx);
            cx.stop_propagation();
        }
    }

    /// Recompute the keymap and hand it to the process key bindings and menus.
    pub fn sync_bindings(&mut self, cx: &mut Context<Self>) {
        self.keymap = settings::effective_keymap(&self.ghostty, self.prefs.ghostty_keys);
        self.chords = shown_chords(&self.keymap);
        // Every Ghostty config change resyncs the bindings, so the look follows it too.
        *self.paint.get_mut() = None;
        let keymap = self.keymap.clone();
        let ghostty = self.ghostty.clone();
        let keys = self.prefs.ghostty_keys;
        self.defer_desktop(cx, move |desktop, cx| {
            desktop.install_bindings(keymap, &ghostty, keys, cx)
        });
    }

    /// Kit controls (buttons, inputs, palette, settings) paint with the phux theme.
    pub(crate) fn apply_kit_theme(&self, cx: &mut Context<Self>) {
        let colors = self.palette();
        let skin = *self.skin_target;
        cx.defer(move |cx| super::super::color::apply_kit_theme(&colors, &skin, cx));
    }
}
