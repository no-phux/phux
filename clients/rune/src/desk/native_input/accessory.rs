//! Synchronous drawn keys for app-owned sheet fields in either phone host.
//! The system keyboard owns native field shift/control editing traits.
use super::super::questions::Owner;
use super::super::{Desk, input::Modal, phone::Soft};
use super::target::Field;
use rune::{KeyEvent, Runtime, Tx};
use std::collections::HashSet;

pub(crate) fn editing(d: &Desk) -> bool {
    d.modal().is_some()
        || d.inbox
            .questions
            .drafts
            .values()
            .any(|draft| draft.texts.values().any(|edit| edit.focused))
}

pub(crate) fn done(d: &mut Desk, tx: &mut Tx) {
    if d.modal() == Some(Modal::PhoneNavigation) {
        d.blur_phone_search(tx);
        return;
    }
    if d.settings_open {
        d.blur_settings_search(tx);
        return;
    }
    for edit in [
        &mut d.palette_query,
        &mut d.connection.find_query,
        &mut d.connection.tabs.rename_field,
        &mut d.inbox.reply,
        &mut d.machines.field,
    ] {
        edit.focused = false;
        edit.cancel_composition();
    }
    for draft in d.inbox.questions.drafts.values_mut() {
        for edit in draft.texts.values_mut() {
            edit.focused = false;
            edit.cancel_composition();
        }
    }
    // Unfocusing hides the field's keyboard. Whether the terminal shows one
    // is its own choice, and still stands when this sheet closes.
    if let Some(phone) = &mut d.phone {
        phone.release();
    }
    d.connection.preedit = None;
    // A sheet's focus trap would immediately refocus its first editor after
    // blur. Keep focus on the post-modal Done control while hiding input.
    // The focusable control receives pointer focus before this handler. Do
    // not blur it: that would reactivate the sheet's first-field focus trap.
    tx.instant();
}

pub(in crate::desk) fn soft(d: &mut Desk, soft: &Soft, tx: &mut Tx) -> bool {
    let field = match d.modal() {
        Some(Modal::Rename) => Some(Field::Rename),
        Some(Modal::Machines) => Some(Field::Machine),
        Some(Modal::Inbox)
            if d.inbox.reply.focused && Field::resolve(d, "inbox-reply").is_some() =>
        {
            Some(Field::Inbox)
        }
        Some(Modal::Inbox) => return inbox(d, soft, tx),
        None => return question(d, soft, tx),
        _ => return false, // existing Settings/Palette/Find/confirm routes
    };
    if let Some(field) = field {
        send(d, field, soft, tx);
    }
    true
}

fn inbox(d: &mut Desk, soft: &Soft, tx: &mut Tx) -> bool {
    // Only the native bridge retires unmounted ACP focus. A retained draft
    // flag alone must not enable a hidden ACP editor in the drawn preview.
    if d.native.is_some() && question(d, soft, tx) {
        return true;
    }
    let Some(phone) = &mut d.phone else {
        return true;
    };
    let event = crate::keys::plain(phone.for_field(soft.clone()));
    // Navigation/close still works without a mounted reply editor. Other
    // keys are consumed, never inserted into a hidden draft or terminal.
    d.inbox_key(&event, tx);
    true
}

fn send(d: &mut Desk, field: Field, soft: &Soft, tx: &mut Tx) {
    let Some(phone) = &mut d.phone else { return };
    let event = crate::keys::plain(phone.for_field(soft.clone()));
    field.key(d, &event, tx);
}

fn question(d: &mut Desk, soft: &Soft, tx: &mut Tx) -> bool {
    let focused: Vec<_> = d
        .inbox
        .questions
        .drafts
        .iter()
        .flat_map(|(question, draft)| {
            draft
                .texts
                .iter()
                .filter(|(_, edit)| edit.focused)
                .map(move |(field, _)| question.field_key(field))
        })
        .collect();
    let [key] = focused.as_slice() else {
        return !focused.is_empty();
    };
    // A held/retired/ambiguous request consumes the key without sending it.
    if let Some((field, _)) = Field::question(d, key) {
        if let Field::Question { owner, .. } = &field
            && d.modal().is_none()
            && !d
                .connection
                .shown
                .as_ref()
                .is_some_and(|shown| &shown.resource == owner.terminal())
        {
            return true;
        }
        send(d, field, soft, tx);
    }
    true
}

/// Retained ACP drafts survive navigation, but their focus must not. Rune
/// does not clear the model's flag when its bound field is unmounted. Keep
/// mounted held fields fail-closed; retire only fields absent from the view.
pub(super) fn retire_unmounted(rt: &mut Runtime<Desk>) -> bool {
    if rt.state.native.is_none() {
        return false;
    }
    let mounted: HashSet<_> = rt
        .elements()
        .into_iter()
        .filter(|e| e.role == "textbox" && !e.exiting)
        .filter_map(|e| e.key)
        .collect();
    let hidden = unmounted_fields(&rt.state, &mounted);
    if hidden.is_empty() {
        return false;
    }
    rt.update(|d, tx| {
        for (question, field) in hidden {
            if let Some(edit) = d
                .inbox
                .questions
                .drafts
                .get_mut(&question)
                .and_then(|draft| draft.texts.get_mut(&field))
            {
                edit.focused = false;
                edit.cancel_composition();
            }
        }
        tx.instant();
    });
    true
}

fn unmounted_fields(d: &Desk, mounted: &HashSet<String>) -> Vec<(Owner, String)> {
    let mut hidden = Vec::new();
    for (question, draft) in &d.inbox.questions.drafts {
        for (field, edit) in &draft.texts {
            if (edit.focused || edit.is_composing())
                && !mounted.contains(&question.field_key(field))
            {
                hidden.push((question.clone(), field.clone()));
            }
        }
    }
    hidden
}

fn held(d: &Desk) -> bool {
    d.phone
        .as_ref()
        .is_some_and(|p| p.ctrl || p.alt || p.shift || p.capital)
}

pub(super) fn text(d: &mut Desk, text: String, tx: &mut Tx) {
    d.connection.preedit = None;
    if held(d) && text.chars().count() == 1 {
        if let Some(c) = text.chars().next() {
            d.soft(Soft::Char(c), tx);
        }
    } else {
        // An IME/dictation phrase is committed Unicode, not a sequence of
        // physical modifier chords. Consume one-shot latches without splitting
        // its graphemes or leaving a stale Control waiting for the next key.
        if let Some(phone) = &mut d.phone {
            phone.release();
        }
        d.ime(&rune::Ime::Commit(text));
    }
}

pub(super) fn key(d: &mut Desk, event: KeyEvent, tx: &mut Tx) {
    if !held(d) || event.mods.cmd {
        d.key(&event, tx);
        return;
    }
    if let Some(phone) = &mut d.phone {
        phone.ctrl |= event.mods.ctrl;
        phone.alt |= event.mods.alt;
        phone.shift |= event.mods.shift;
    }
    d.soft(Soft::Key(event.key.clone()), tx);
}
