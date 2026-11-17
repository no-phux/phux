//! The bar a held action waits in, and the question it asks.

use rune_kit::{Button, ButtonVariant};

use super::*;

/// What a held action would do, and the word for doing it. A keyboard is
/// told its key; `touch` has none to name.
pub(super) fn question(pending: &Pending, touch: bool) -> (String, &'static str) {
    let keyed = |bare: &'static str, keyed: &'static str| if touch { bare } else { keyed };
    match pending {
        Pending::Mirror(source, target) => (
            format!(
                "Start {} mirror of {target:?} in a new producer terminal? {} Read-only; no browser/app input or process migration.",
                source.verb(),
                source.prerequisites()
            ),
            "Start",
        ),
        Pending::Paste(_, held) => {
            let lines = held.lines().count().max(1);
            let plural = if lines == 1 { "" } else { "s" };
            (
                format!(
                    "This paste has {lines} line{plural} and would run as typed commands. Paste anyway?"
                ),
                keyed("Paste", "Paste (Return)"),
            )
        }
        Pending::Close(terminal) => (
            format!("Close {terminal}? Anything running in it ends."),
            keyed("Close", "Close (Return)"),
        ),
        Pending::CloseTab(terminals) if terminals.len() == 1 => (
            "Close this tab? Anything running in it ends.".to_owned(),
            keyed("Close", "Close (Return)"),
        ),
        Pending::CloseTab(terminals) => (
            format!(
                "Close this tab and its {} terminals? Anything running in them ends.",
                terminals.len()
            ),
            keyed("Close", "Close (Return)"),
        ),
        Pending::ClipboardRead(terminal, _) => (
            format!(
                "{terminal} wants to read your clipboard.{}",
                keyed("", " A allows it every time.")
            ),
            keyed("Allow once", "Allow once (Return)"),
        ),
        Pending::CloseSession(_, name) => (
            format!("End session {name} and everything running in it?"),
            keyed("End session", "End session (Return)"),
        ),
    }
}

/// Whether confirming `pending` ends something: the destructive verbs
/// (Close, End session) draw as the danger button (§1.10).
pub(super) const fn destructive(pending: &Pending) -> bool {
    matches!(
        pending,
        Pending::Close(_) | Pending::CloseTab(_) | Pending::CloseSession(..)
    )
}

/// The action waiting on Return or Escape, floating over the terminals so
/// asking never resizes them.
pub fn confirm_bar(desk: &Desk, p: &Tokens) -> Option<El> {
    // A popup covers this place and asks in its own box.
    if desk.connection.popup.command().is_some() {
        return None;
    }
    question_bar(desk, p)
}

/// [`confirm_bar`], wherever its parent puts it: an opaque floating
/// surface (it is past the blur budget, §1.9), Cancel outlined and the verb
/// the view's one primary action, or the danger button when it ends
/// something.
pub(in crate::ui) fn question_bar(desk: &Desk, p: &Tokens) -> Option<El> {
    let t = p;
    let s = &t.space;
    let pending = desk.pending.as_ref()?;
    let touch = desk.phone.is_some();
    let (message, verb) = question(pending, touch);
    let cancel = if touch { "Cancel" } else { "Cancel (Esc)" };
    let verb_look = if destructive(pending) {
        ButtonVariant::Danger
    } else {
        ButtonVariant::Submit
    };
    let cancel = Button::new(cancel)
        .variant(ButtonVariant::Outline)
        .on_press(Desk::cancel_pending)
        .build(&t.kit)
        .key("confirm-cancel");
    let ok = Button::new(verb)
        .variant(verb_look)
        .on_press_tx(Desk::confirm_pending_tx)
        .build(&t.kit)
        .key("confirm-ok");
    let surface = row()
        .key("confirm-bar")
        .on_drag(keep_from_pane)
        .pin(Some(0.0), Some(0.0), Some(0.0), None)
        .px(s.card_inset)
        .py(s.s2)
        .gap(s.s2)
        .items_center()
        .radius(t.radius.lg)
        .continuous_corners()
        .fill(t.roles.layer.raised.paint())
        .border(s.hairline, t.roles.border.base.paint())
        .transition(rune::Transition::opacity())
        .child(
            text(message)
                .font(t.ty.label)
                .color(t.roles.text.text.paint())
                .wrap()
                .grow(1.0)
                .min_w(0.0),
        )
        .child(touchable(desk, cancel))
        .child(touchable(desk, ok));
    Some(t.elevate(surface, Tier::Floating))
}
