//! Target-bound synchronous native input. UIKit can ask about its document
//! during a callback: never queue an unaddressed edit for a later Rune frame.
use rune::{Key, KeyEvent, Mods, Runtime};
use serde::{Deserialize, Serialize};

use super::Desk;
use target::Target;

mod accessory;
mod target;
pub(in crate::desk) use accessory::soft as accessory;
pub(crate) use accessory::{done, editing};
#[cfg(test)]
mod tests;
mod unicode;

#[derive(Clone, Serialize, PartialEq)]
pub struct Snapshot {
    pub token: u64,
    pub revision: u64,
    pub kind: &'static str,
    pub text: String,
    pub anchor: usize,
    pub focus: usize,
    pub multiline: bool,
    pub option_as_meta: bool,
    pub wanted: bool,
    pub caret: Option<[f32; 4]>,
}

#[derive(Deserialize)]
pub struct Operation {
    pub token: u64,
    pub revision: u64,
    #[serde(flatten)]
    pub action: Action,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Action {
    Replace {
        expected: String,
        text: String,
        anchor: usize,
        focus: usize,
    },
    Preedit {
        text: String,
        anchor: usize,
        focus: usize,
    },
    Selection {
        anchor: usize,
        focus: usize,
    },
    Cancel,
    Dismiss,
    Copy,
    SelectAll,
    Text {
        text: String,
    },
    Paste {
        text: String,
    },
    Key {
        name: String,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        alt: bool,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        command: bool,
    },
}

#[derive(Default)]
pub struct Bridge {
    target: Option<Target>,
    previous: Option<Snapshot>,
    token: u64,
    revision: u64,
}

impl Bridge {
    /// Retire only transient composition when the responder's owner changes.
    /// Direct app-model preedit is not tracked by Rune's queued IME hook.
    pub fn reconcile(&mut self, rt: &mut Runtime<Desk>) -> bool {
        let retired = accessory::retire_unmounted(rt);
        let changed = self.target != target::resolve(rt);
        if changed {
            self.cancel_preedit(rt);
        }
        retired || changed
    }

    pub fn cancel_preedit(&self, rt: &mut Runtime<Desk>) {
        rt.update(|d, tx| {
            if let Some(Target::Field(field, _)) = &self.target
                && let Some(edit) = field.edit_mut(d)
            {
                edit.cancel_composition();
            }
            d.preedit = None;
            if let Some(phone) = &mut d.phone {
                phone.release();
            }
            tx.instant();
        });
    }

    pub fn retire(&mut self) {
        self.target = None;
        self.previous = None;
        self.token += 1;
    }

    pub fn snapshot(&mut self, rt: &Runtime<Desk>) -> Snapshot {
        let target = target::resolve(rt);
        if target != self.target {
            self.token += 1;
            self.revision = 0;
            self.previous = None;
            self.target = target;
        }
        let (kind, edit, multiline) = match &self.target {
            Some(Target::Field(field, _)) => ("field", field.edit(&rt.state), field.multiline()),
            Some(Target::Terminal(_)) => ("terminal", None, false),
            Some(Target::Control(_)) => ("control", None, false),
            None => ("none", None, false),
        };
        let text = edit.map_or_else(String::new, |e| e.text.clone());
        let anchor = edit.map_or(0, |e| e.text[..e.anchor].encode_utf16().count());
        let focus = edit.map_or(0, |e| e.text[..e.focus].encode_utf16().count());
        if self
            .previous
            .as_ref()
            .is_some_and(|p| p.text != text || p.anchor != anchor || p.focus != focus)
        {
            self.revision += 1;
        }
        let wanted =
            self.target.is_some() && (kind == "field" || rt.state.phone_keyboard_visible());
        let snapshot = Snapshot {
            token: self.token,
            revision: self.revision,
            kind,
            text,
            anchor,
            focus,
            multiline,
            option_as_meta: rt.state.settings.option_as_meta,
            wanted,
            caret: if kind == "terminal" {
                terminal_caret(&rt.state)
            } else {
                rt.ime_area
            },
        };
        self.previous = Some(snapshot.clone());
        snapshot
    }

    pub fn apply(&mut self, rt: &mut Runtime<Desk>, operation: Operation) -> Result<bool, String> {
        self.reconcile(rt);
        let snapshot = self.snapshot(rt);
        if operation.token != snapshot.token || operation.revision != snapshot.revision {
            return Ok(false);
        }
        let Some(target) = self.target.clone() else {
            return Ok(false);
        };
        if matches!(operation.action, Action::Dismiss) {
            dismiss(rt, &target);
            self.retire();
            return Ok(true);
        }
        let accepted = match target {
            Target::Field(field, _) => self.field(rt, &field, operation.action)?,
            Target::Terminal(_) => self.terminal(rt, operation.action)?,
            Target::Control(_) => self.control(rt, operation.action)?,
        };
        self.snapshot(rt);
        Ok(accepted)
    }

    fn field(
        &self,
        rt: &mut Runtime<Desk>,
        field: &target::Field,
        action: Action,
    ) -> Result<bool, String> {
        if let Action::Key {
            name,
            ctrl,
            alt,
            shift,
            command,
        } = action
        {
            let event = event(&name, ctrl, alt, shift, command)?;
            rt.update(|d, tx| {
                field.key(d, &event, tx);
                tx.instant();
            });
            return Ok(true);
        }
        let Some(original) = field.edit(&rt.state) else {
            return Ok(false);
        };
        let before = original.text.clone();
        let mut edited = original.clone();
        unicode::edit(&mut edited, action)?;
        rt.update(|d, tx| {
            let Some(model) = field.edit_mut(d) else {
                return;
            };
            *model = edited;
            if model.text != before {
                field.changed(d);
            }
            tx.instant();
        });
        Ok(true)
    }

    fn terminal(&self, rt: &mut Runtime<Desk>, action: Action) -> Result<bool, String> {
        match action {
            Action::Copy => rt.update(|d, tx| d.copy(tx)),
            Action::SelectAll => rt.update(|d, tx| d.select_all(tx)),
            Action::Text { text } => {
                rt.update(|d, tx| {
                    accessory::text(d, text, tx);
                });
            }
            Action::Preedit { text, .. } => {
                rt.update(|d, _| {
                    d.ime(&rune::Ime::Preedit(text, None));
                });
            }
            Action::Cancel => rt.update(|d, _| d.preedit = None),
            Action::Paste { text } => rt.update(|d, _| {
                if let Some(id) = d.shown.as_ref().map(|s| s.terminal.clone()) {
                    d.paste(&id, &text);
                }
            }),
            Action::Key {
                name,
                ctrl,
                alt,
                shift,
                command,
            } => {
                let event = event(&name, ctrl, alt, shift, command)?;
                rt.update(|d, tx| {
                    accessory::key(d, event, tx);
                });
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn control(&self, rt: &mut Runtime<Desk>, action: Action) -> Result<bool, String> {
        let Action::Key {
            name,
            ctrl,
            alt,
            shift,
            command,
        } = action
        else {
            return Ok(false);
        };
        let event = event(&name, ctrl, alt, shift, command)?;
        rt.update(|d, tx| {
            d.key(&event, tx);
        });
        Ok(true)
    }
}

fn terminal_caret(d: &Desk) -> Option<[f32; 4]> {
    let shown = d.shown.as_ref()?;
    let origin = d.origins.get(&shown.terminal)?;
    let frame = d.client.as_ref()?.acquire_view(shown.view?)?;
    let metrics = d.metrics();
    Some([
        origin[0] + f32::from(frame.cursor.col) * metrics.cell_w,
        origin[1] + f32::from(frame.cursor.row) * metrics.cell_h,
        metrics.cell_w,
        metrics.cell_h,
    ])
}

fn dismiss(rt: &mut Runtime<Desk>, target: &Target) {
    rt.update(|d, tx| {
        d.preedit = None;
        if let Target::Field(field, _) = target
            && let Some(edit) = field.edit_mut(d)
        {
            edit.focused = false;
            edit.cancel_composition();
        }
        // Dismissing a field's keyboard says nothing about the terminal's.
        if matches!(target, Target::Terminal(_))
            && let Some(phone) = &mut d.phone
        {
            phone.keyboard = false;
        }
        tx.blur();
    });
}

fn event(
    name: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    command: bool,
) -> Result<KeyEvent, String> {
    let key = match name {
        "Enter" => Key::Enter,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "Escape" => Key::Escape,
        "Tab" => Key::Tab,
        "Up" => Key::Up,
        "Down" => Key::Down,
        "Left" => Key::Left,
        "Right" => Key::Right,
        "Home" => Key::Home,
        "End" => Key::End,
        s if s.chars().count() == 1 && command => {
            Key::Command(s.chars().next().ok_or("Missing key")?)
        }
        s if s.chars().count() == 1 => Key::Text(s.to_owned()),
        _ => return Err("Unknown native key.".into()),
    };
    Ok(KeyEvent {
        key,
        mods: Mods {
            ctrl,
            alt,
            shift,
            cmd: command,
            logo: command,
        },
    })
}
