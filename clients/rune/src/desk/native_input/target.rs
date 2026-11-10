//! Finite app-owned editing targets. Unknown or inert fields fail closed.
use phux_protocol::ids::ResourceId;
use rune::{Key, KeyEvent, Runtime, TextEdit, Tx};

use super::super::{Desk, input::Modal};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Palette,
    Find,
    Settings,
    Rename,
    Inbox,
    Machine,
    Question {
        terminal: ResourceId,
        question: String,
        field: String,
        multiline: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Field(Field, String),
    Terminal(String),
    Control(String),
}

impl Field {
    fn resolve(d: &Desk, key: &str) -> Option<(Self, String)> {
        let found = match (key, d.modal()) {
            ("palette-query", Some(Modal::Palette)) => {
                (Self::Palette, format!("{:?}", d.palette_mode))
            }
            ("find-query", Some(Modal::FindField)) => (
                Self::Find,
                format!("{:?}", d.shown.as_ref().map(|s| (&s.resource, s.view))),
            ),
            ("settings-query", Some(Modal::Settings)) if d.settings_search_focused() => {
                (Self::Settings, "settings".into())
            }
            ("rename-field", Some(Modal::Rename)) => {
                (Self::Rename, format!("{:?}", d.tabs.renaming))
            }
            ("inbox-reply", Some(Modal::Inbox)) => (Self::Inbox, inbox_owner(d)?),
            ("machine-field", Some(Modal::Machines)) => (Self::Machine, "machine".into()),
            _ if matches!(d.modal(), None | Some(Modal::Inbox)) => return Self::question(d, key),
            _ => return None,
        };
        Some(found)
    }

    pub(super) fn question(d: &Desk, key: &str) -> Option<(Self, String)> {
        let mut resolved = None;
        for (terminal, (session, view)) in &d.inbox.questions.views {
            for question in &view.questions {
                if d.inbox.questions.held(view, &question.id) {
                    continue;
                }
                let Some(draft) = d.inbox.questions.drafts.get(&question.id) else {
                    continue;
                };
                if draft.question != *question {
                    continue;
                }
                let Some(field) = draft
                    .texts
                    .keys()
                    .find(|field| key == format!("acp-{}-field-{field}", question.id))
                else {
                    continue;
                };
                // Question IDs are activation-scoped, not global. If two
                // visible requests share this widget key, never guess which
                // terminal should receive a native callback.
                if resolved.is_some() {
                    return None;
                }
                resolved = Some((
                    Self::Question {
                        terminal: terminal.clone(),
                        question: question.id.clone(),
                        field: field.clone(),
                        multiline: question.kind == "editor",
                    },
                    format!(
                        "{terminal}:{session}:{}:{}",
                        view.activation,
                        serde_json::to_string(question).ok()?
                    ),
                ));
            }
        }
        resolved
    }

    pub(super) fn edit<'a>(&self, d: &'a Desk) -> Option<&'a TextEdit> {
        Some(match self {
            Self::Palette => &d.palette_query,
            Self::Find => &d.find_query,
            Self::Settings => &d.settings_ui.query,
            Self::Rename => &d.tabs.rename_field,
            Self::Inbox => &d.inbox.reply,
            Self::Machine => &d.machines.field,
            Self::Question {
                question, field, ..
            } => d.inbox.questions.drafts.get(question)?.texts.get(field)?,
        })
    }

    pub(super) fn edit_mut<'a>(&self, d: &'a mut Desk) -> Option<&'a mut TextEdit> {
        Some(match self {
            Self::Palette => &mut d.palette_query,
            Self::Find => &mut d.find_query,
            Self::Settings => &mut d.settings_ui.query,
            Self::Rename => &mut d.tabs.rename_field,
            Self::Inbox => &mut d.inbox.reply,
            Self::Machine => &mut d.machines.field,
            Self::Question {
                question, field, ..
            } => d
                .inbox
                .questions
                .drafts
                .get_mut(question)?
                .texts
                .get_mut(field)?,
        })
    }

    pub(super) fn changed(&self, d: &mut Desk) {
        match self {
            Self::Palette => d.palette_edited(),
            Self::Find => d.run_search(),
            _ => {}
        }
    }

    pub(super) fn multiline(&self) -> bool {
        matches!(
            self,
            Self::Question {
                multiline: true,
                ..
            }
        )
    }

    pub(super) fn key(&self, d: &mut Desk, event: &KeyEvent, tx: &mut Tx) {
        let taken = match self {
            Self::Palette => d.palette_key(event, tx),
            Self::Find => d.find_key(event, tx),
            Self::Settings => d.settings_field_key(event, tx),
            Self::Rename => d.rename_key(event, tx),
            Self::Inbox => d.inbox_key(event, tx),
            Self::Machine => d.machines_key(event, tx),
            Self::Question { .. } => false,
        };
        if taken {
            return;
        }
        if event.key == Key::Enter && !self.multiline() {
            return self.submit(d, tx);
        }
        if let Some(edit) = self.edit_mut(d) {
            let before = edit.text.clone();
            let outcome = edit.key(event, None);
            if outcome.submit && self.multiline() {
                edit.insert("\n");
            }
            if outcome.blur {
                edit.focused = false;
                tx.blur();
            }
            if edit.text != before {
                self.changed(d);
            }
        }
    }

    fn submit(&self, d: &mut Desk, tx: &mut Tx) {
        match self {
            Self::Palette => d.palette_run(None, tx),
            Self::Rename => d.submit_rename(tx),
            Self::Machine => d.add_typed_machine(),
            Self::Question {
                terminal, question, ..
            } => d.answer_question(terminal, question, None),
            _ => {}
        }
    }
}

pub(super) fn resolve(rt: &Runtime<Desk>) -> Option<Target> {
    let d = &rt.state;
    if d.native.as_ref().is_some_and(|n| !n.active) {
        return None;
    }
    let connection = format!(
        "{}:{}:{:?}",
        d.link.server,
        d.epoch,
        d.client.as_ref().map(std::sync::Arc::as_ptr)
    );
    if let Some(control) = control(d, &connection) {
        return Some(control);
    }
    if let Some(element) = rt
        .elements()
        .into_iter()
        .find(|e| e.focused && !e.inert && !e.exiting)
    {
        return focused_field(d, element, &connection);
    }
    match d.modal() {
        Some(_) => None,
        None => terminal(d, &connection),
    }
}

fn terminal(d: &Desk, connection: &str) -> Option<Target> {
    if d.native.is_some() && super::accessory::editing(d) {
        return None;
    }
    let shown = d.shown.as_ref()?;
    Some(Target::Terminal(format!(
        "{connection}:{}:{:?}",
        shown.resource, shown.view?
    )))
}

fn focused_field(d: &Desk, element: rune::ElementInfo, connection: &str) -> Option<Target> {
    if element.role != "textbox" {
        return None;
    }
    let (field, owner) = Field::resolve(d, element.key.as_deref()?)?;
    Some(Target::Field(
        field,
        format!("{connection}:{owner}:{}", element.path),
    ))
}

fn control(d: &Desk, connection: &str) -> Option<Target> {
    let owner = match d.modal()? {
        Modal::Confirm => format!("confirm:{:?}", d.pending),
        Modal::PhonePicker => "picker".into(),
        _ => return None,
    };
    Some(Target::Control(format!("{connection}:{owner}")))
}

fn inbox_owner(d: &Desk) -> Option<String> {
    let items = d.inbox_items();
    let crate::inbox::Item::Terminal(waiting) = items.get(d.picked_index(&items)?)? else {
        return None;
    };
    let ask = waiting.ask.as_ref()?;
    if !ask.typed() {
        return None;
    }
    Some(format!("{}:{}:{ask:?}", waiting.terminal, waiting.session))
}
