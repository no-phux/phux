//! The bar a held action waits in, and the question it asks.

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

/// The action waiting on Return or Escape, floating over the terminals so
/// asking never resizes them.
pub fn confirm_bar(desk: &Desk, p: &Ink) -> Option<El> {
    // A popup covers this place and asks in its own box.
    if desk.popup.command().is_some() {
        return None;
    }
    question_bar(desk, p)
}

/// [`confirm_bar`], wherever its parent puts it.
pub(in crate::ui) fn question_bar(desk: &Desk, p: &Ink) -> Option<El> {
    let touch = desk.phone.is_some();
    let (message, verb) = question(desk.pending.as_ref()?, touch);
    let cancel = if touch { "Cancel" } else { "Cancel (Esc)" };
    let press = |button: El| touchable(desk, button.grow(0.0).px(10.0));
    Some(
        row()
            .key("confirm-bar")
            .on_drag(keep_from_pane)
            .pin(Some(0.0), Some(0.0), Some(0.0), None)
            .px(12.0)
            .py(8.0)
            .gap(8.0)
            .items_center()
            .radius(crate::ui::CARD_RADIUS)
            .continuous_corners()
            .backdrop_blur(crate::ui::FROST)
            .fill(p.raised.alpha(0.82))
            .border(1.0, p.edge)
            .shadow(6.0, 18.0, Color::BLACK.alpha(0.35))
            .transition(rune::Transition::opacity())
            .child(
                text(message)
                    .font(LABEL)
                    .color(p.text)
                    .wrap()
                    .grow(1.0)
                    .min_w(0.0),
            )
            .child(press(button(
                cancel,
                "confirm-cancel",
                p,
                Desk::cancel_pending,
            )))
            .child(press(
                button(verb, "confirm-ok", p, Desk::confirm_pending)
                    .on_press_tx(Desk::confirm_pending_tx),
            )),
    )
}
