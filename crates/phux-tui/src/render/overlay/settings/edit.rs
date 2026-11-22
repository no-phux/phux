//! Edits: what each key or click writes, undo, and the value editor.

use super::{
    Edit, InputState, KeyEvent, LayerSource, ModSet, OverlayCommand, PhysicalKey, SettingKind,
    SettingSpec, SettingsOverlay, Status, Undo, kit, parse_input, render_value,
};

impl SettingsOverlay {
    // ---- editing --------------------------------------------------------

    /// Enter, Space, or a click: the row's primary action.
    pub(super) fn activate(&mut self, spec: &'static SettingSpec) -> OverlayCommand {
        match spec.kind {
            SettingKind::Bool => {
                let current = self.value_at(spec.key).and_then(|v| v.as_bool());
                self.apply(
                    spec,
                    Edit::Set(toml::Value::Boolean(!current.unwrap_or(false))),
                )
            }
            SettingKind::OptionalBool | SettingKind::Choice(_) => self.cycle(spec, 1),
            _ => {
                let text = self
                    .effective(spec)
                    .map(|v| render_value(Some(&v)))
                    .unwrap_or_default();
                self.model.input = InputState::new(text);
                self.editor = Some(spec.key);
                self.status = None;
                OverlayCommand::Stay
            }
        }
    }

    /// Left/Right: step a choice, a tri-state, a bool, or an integer.
    pub(super) fn cycle(&mut self, spec: &'static SettingSpec, direction: i64) -> OverlayCommand {
        match spec.kind {
            SettingKind::Bool => self.activate(spec),
            SettingKind::Choice(variants) => {
                let current = self.value_at(spec.key);
                let current = current.as_ref().and_then(toml::Value::as_str);
                let at = current
                    .and_then(|c| variants.iter().position(|v| *v == c))
                    .unwrap_or(0);
                let len = variants.len();
                let next = if direction >= 0 {
                    (at + 1) % len
                } else {
                    (at + len - 1) % len
                };
                self.apply(
                    spec,
                    Edit::Set(toml::Value::String(variants[next].to_owned())),
                )
            }
            SettingKind::OptionalBool => {
                // unset -> true -> false -> unset, and back the other way.
                let current = self.value_at(spec.key).and_then(|v| v.as_bool());
                let next = match (current, direction >= 0) {
                    (None, true) | (Some(false), false) => Edit::Set(toml::Value::Boolean(true)),
                    (Some(true), true) | (None, false) => Edit::Set(toml::Value::Boolean(false)),
                    (Some(false), true) | (Some(true), false) => Edit::Unset,
                };
                self.apply(spec, next)
            }
            SettingKind::Integer { min, max } | SettingKind::OptionalInteger { min, max } => {
                let Some(current) = self
                    .value_at(spec.key)
                    .or_else(|| self.default_at(spec))
                    .and_then(|v| v.as_integer())
                else {
                    return self.activate(spec);
                };
                let next = current.saturating_add(direction).clamp(min, max);
                if next == current {
                    return OverlayCommand::Stay;
                }
                self.apply(spec, Edit::Set(toml::Value::Integer(next)))
            }
            _ => OverlayCommand::Stay,
        }
    }

    /// Delete / `C-r`: remove the override so the shipped default shows
    /// through -- only when the override is in the user's own file.
    pub(super) fn reset(&mut self, spec: &'static SettingSpec) -> OverlayCommand {
        match self.origin_at(spec.key) {
            None => {
                self.status = Some(Status::Note(format!(
                    "{} is already at its shipped default",
                    spec.key
                )));
                OverlayCommand::Stay
            }
            Some(LayerSource::Extended(layer)) => {
                self.status = Some(Status::Refused(format!(
                    "{} is set by the extends layer {}; edit that file to change it",
                    spec.key,
                    layer.display()
                )));
                OverlayCommand::Stay
            }
            Some(LayerSource::User(_) | LayerSource::Defaults) => self.apply(spec, Edit::Unset),
        }
    }

    /// `C-z`: put the last edited key back the way the file had it.
    pub(super) fn undo_last(&mut self) -> OverlayCommand {
        let Some(undo) = self.undo.pop() else {
            self.status = Some(Status::Note("nothing to undo".to_owned()));
            return OverlayCommand::Stay;
        };
        match phux_config::settings::write_edit(&self.path, undo.key, undo.restore) {
            Ok(_) => {
                self.reload_snapshot();
                self.status = Some(Status::Saved(format!(
                    "undid the last change to {}",
                    undo.key
                )));
                OverlayCommand::ReloadConfig
            }
            Err(err) => {
                self.status = Some(Status::Refused(err.to_string()));
                OverlayCommand::Stay
            }
        }
    }

    /// Write one edit, re-read the file, and ask the driver to reload.
    pub(super) fn apply(&mut self, spec: &'static SettingSpec, edit: Edit) -> OverlayCommand {
        match phux_config::settings::write_edit(&self.path, spec.key, edit) {
            Ok(outcome) => {
                self.undo.push(Undo {
                    key: spec.key,
                    restore: outcome.previous.map_or(Edit::Unset, Edit::Set),
                });
                self.reload_snapshot();
                self.status = Some(Status::Saved(outcome.line.map_or_else(
                    || self.removed_message(spec),
                    |line| format!("saved [{}] {line}", spec.table()),
                )));
                OverlayCommand::ReloadConfig
            }
            Err(err) => {
                self.status = Some(Status::Refused(err.to_string()));
                OverlayCommand::Stay
            }
        }
    }

    /// What an `Unset` left behind: the shipped default, or -- when an
    /// `extends` layer also sets the key -- that layer's value.
    pub(super) fn removed_message(&self, spec: &SettingSpec) -> String {
        match self.origin_at(spec.key) {
            Some(LayerSource::Extended(layer)) => format!(
                "removed your override of {}; layer {} now sets it to {}",
                spec.key,
                layer.display(),
                render_value(self.effective(spec).as_ref())
            ),
            _ => format!(
                "removed {} (shipped default {} applies)",
                spec.key,
                render_value(self.default_at(spec).as_ref())
            ),
        }
    }

    /// Enter in the editor: parse the text for the row's kind and write.
    pub(super) fn commit_editor(&mut self) -> OverlayCommand {
        let Some(key) = self.editor.take() else {
            return OverlayCommand::Stay;
        };
        let Some(spec) = self.specs.iter().copied().find(|s| s.key == key) else {
            return OverlayCommand::Stay;
        };
        match parse_input(spec.kind, self.model.input.value().trim()) {
            Ok(edit) => self.apply(spec, edit),
            Err(message) => {
                self.status = Some(Status::Refused(message));
                // Keep the editor open with the text so the user can fix it.
                self.editor = Some(key);
                OverlayCommand::Stay
            }
        }
    }

    /// The editor's keys: Esc closes it, Enter commits, `C-u` clears the
    /// text (ratcn's field leaves it unbound); the rest is ratcn's
    /// [`Input`], the line editor.
    pub(super) fn handle_editor_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        match key.key {
            PhysicalKey::Escape => {
                self.editor = None;
                OverlayCommand::Stay
            }
            PhysicalKey::Enter | PhysicalKey::NumpadEnter => self.commit_editor(),
            PhysicalKey::U if key.mods.contains(ModSet::CTRL) => {
                self.model.input = InputState::default();
                OverlayCommand::Stay
            }
            _ => kit::key(key).map_or(OverlayCommand::Stay, |event| self.route(event).0),
        }
    }
}
