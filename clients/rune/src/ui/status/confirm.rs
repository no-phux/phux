//! The bar a held action waits in, and the question it asks.

use phux_protocol::ids::ResourceId;
use rune_kit::{Button, ButtonVariant};

use crate::ui::tokens::layout::OVERLAY;

use super::*;

/// What a held action would do, and the word for doing it. A keyboard is
/// told its key; `touch` has none to name. `called` names a terminal.
pub(super) fn question(
    pending: &Pending,
    touch: bool,
    called: &dyn Fn(&ResourceId) -> String,
) -> (String, &'static str) {
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
            format!("Close {}? Anything running in it ends.", called(terminal)),
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
                "{} wants to read your clipboard.{}",
                called(terminal),
                keyed("", " A allows it every time.")
            ),
            keyed("Allow once", "Allow once (Return)"),
        ),
        Pending::ClipboardWrite(terminal, text) => (
            format!(
                "{} wants to set your clipboard ({} characters).",
                called(terminal),
                text.chars().count()
            ),
            keyed("Allow", "Allow (Return)"),
        ),
        // Its pane by address: a title is the program's to write.
        Pending::OpenUrl(asked) => (
            format!(
                "{} on {} asks to open this link here:",
                asked.terminal, asked.machine
            ),
            keyed("Open", "Open (O)"),
        ),
        Pending::CloseSession(_, name) => (
            format!("End session {name} and everything running in it?"),
            keyed("End session", "End session (Return)"),
        ),
    }
}

/// A terminal as the person knows it, its name beside its address
/// (`build (@4)`), or the address alone when nothing names it. A prompt
/// that ends a terminal or hands it the clipboard says which one.
fn called(desk: &Desk, id: &ResourceId) -> String {
    let named = (desk.connection.topology.as_ref())
        .and_then(|topology| topology.pane(id))
        .and_then(|pane| {
            phux_client_core::pane_label::pane_label(
                pane.name.as_deref(),
                pane.title.as_deref(),
                pane.foreground.as_deref(),
                pane.cwd.as_deref(),
            )
        });
    named.map_or_else(|| id.to_string(), |name| format!("{name} ({id})"))
}

/// The link a pane asks to open: its site, then all of it, scrolling in its
/// own box so the site never leaves the screen, and where a sign-in comes
/// back to.
fn link(asked: &crate::desk::open_url::Asked, t: &Tokens) -> El {
    let ink = t.roles.text.text.paint();
    let note = asked.link.callback_port().zip(asked.remote.as_ref());
    let note = note.map(|(port, machine)| {
        format!(
            "It comes back to port {port} on this machine. To pass that on: \
             phux forward -L {port}:localhost:{port} --remote {machine}"
        )
    });
    let words = |words: String, font| text(words).font(font).color(ink);
    col()
        .w_full()
        .gap(t.space.s1)
        .child(
            words(asked.link.host().to_owned(), t.ty.label.semibold())
                .wrap_anywhere()
                .w_full()
                .key("open-url-site"),
        )
        .child(
            col()
                .w_full()
                .max_h(OVERLAY.link_h)
                .min_h(0.0)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(
                    words(asked.link.as_str().to_owned(), t.ty.label)
                        .wrap_anywhere()
                        .w_full()
                        .key("open-url-link"),
                ),
        )
        .children(note.map(|note| words(note, t.ty.label).wrap().w_full()))
}

/// The pane ids (`@N`) in `message`, set as machine nouns:
/// the mono face, so `@4` reads as an address, not a word.
fn pane_ids(message: &str) -> Vec<rune::Span> {
    let mut spans = Vec::new();
    for (at, _) in message.match_indices('@') {
        let digits = message[at + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .count();
        if digits > 0 {
            spans.push(rune::Span::new(at, at + 1 + digits).mono());
        }
    }
    spans
}

/// Whether confirming `pending` ends something: the destructive verbs
/// (Close, End session) draw as the danger button.
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
/// surface (it is past the blur budget), Cancel outlined and the verb
/// the view's one primary action, or the danger button when it ends
/// something.
pub(in crate::ui) fn question_bar(desk: &Desk, p: &Tokens) -> Option<El> {
    let t = p;
    let s = &t.space;
    let pending = desk.pending.as_ref()?;
    let touch = desk.phone.is_some();
    let (message, verb) = question(pending, touch, &|id| called(desk, id));
    let asked = match pending {
        Pending::OpenUrl(asked) => Some(asked),
        _ => None,
    };
    let cancel = match (touch, asked) {
        (true, _) => "Cancel",
        (false, None) => "Cancel (Esc)",
        (false, Some(_)) => "Don't open (Return)",
    };
    // Declining is a link's primary action.
    let (verb_look, cancel_look) = if destructive(pending) {
        (t.danger_button(), ButtonVariant::Outline)
    } else if asked.is_some() {
        (ButtonVariant::Outline, ButtonVariant::Submit)
    } else {
        (ButtonVariant::Submit, ButtonVariant::Outline)
    };
    let cancel = Button::new(cancel)
        .variant(cancel_look)
        .on_press(Desk::cancel_pending)
        .build(&t.kit)
        .key("confirm-cancel");
    let ok = Button::new(verb)
        .variant(verb_look)
        .on_press_tx(Desk::confirm_pending_tx)
        .build(&t.kit)
        .key("confirm-ok");
    let message = rune::rich(message.clone(), pane_ids(&message))
        .font(t.ty.label)
        .color(t.roles.text.text.paint())
        .wrap();
    let message = match asked {
        None => message,
        Some(asked) => col()
            .gap(s.s1)
            .child(message.w_full())
            .child(link(asked, t)),
    };
    let message = message.grow(1.0).min_w(0.0);
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
        .child(message)
        .child(touchable(desk, cancel))
        .child(touchable(desk, ok));
    Some(t.elevate(surface, Tier::Floating))
}
