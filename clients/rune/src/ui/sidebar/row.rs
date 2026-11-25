//! A sidebar row: one tab, or a terminal no tab holds, on one line.

use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Color, El, Key, el, row, text};

use super::super::rows::{Base, State as Forced, forced_face};
use super::super::{Paint, Tokens, icons, tile};
use super::{drag, face, return_to_terminal_on_click};
use crate::activity::Mark;
use crate::desk::Desk;
use crate::host_color;
use crate::ui::tokens::Voice;

/// A row: one tab of its session, or a resource no tab holds, always below
/// its session's separately actionable header. It reads as the work it
/// holds: who does it (the tile), how it goes (the tile's badge) and what
/// it is (the task, else the program and its folder).
#[derive(Hash)]
pub(super) struct Row<'a> {
    /// The row's lead resource, which names it and keys it.
    pub(super) id: &'a ResourceId,
    /// What a press shows: the pane the tab last had the keyboard in.
    pub(super) target: &'a ResourceId,
    pub(super) session: &'a str,
    pub(super) name: String,
    pub(super) place: Option<String>,
    pub(super) key: Option<SessionKey>,
    pub(super) agent: Option<&'a str>,
    /// The agent's kind, which picks its tile.
    pub(super) kind: Option<&'a str>,
    /// The loudest of its panes' marks.
    pub(super) mark: Mark,
    /// How many panes the tab splits into.
    pub(super) panes: usize,
    pub(super) selected: bool,
    /// How many rows were started from this one, folded under it.
    pub(super) children: usize,
    /// Whether those rows show.
    pub(super) open: bool,
    /// Whether this row was started from another, and sits under it.
    pub(super) child: bool,
    /// The terminal's machine and its colour, so a recolour redraws.
    pub(super) host: (&'a str, u32),
    /// The session's colour, for its quiet mark.
    pub(super) color: Option<u32>,
    /// A look forced for the gallery's states page; `Rest` is the live row.
    pub(super) state: Forced,
}

impl Row<'_> {
    /// What a screen reader and the tooltip add to the name: the agent and
    /// its status word, where it is, and how many panes it holds, so two
    /// rows with one name always read differently. Whether the word leads.
    pub(super) fn detail(&self, t: &Tokens) -> Option<(String, bool)> {
        let word = (self.mark != Mark::None)
            .then(|| face(self.mark, t).word)
            .flatten();
        let doing = match (self.agent, word) {
            (Some(agent), Some(word)) => Some((format!("{agent} · {word}"), true)),
            (None, Some(word)) => Some((word.to_owned(), true)),
            (Some(agent), None) => Some((agent.to_owned(), false)),
            (None, None) => None,
        };
        let split = (self.panes > 1).then(|| format!("{} panes", self.panes));
        let (mut words, tinted) = doing.unwrap_or_default();
        for part in [self.place.clone(), split].into_iter().flatten() {
            if !words.is_empty() {
                words.push_str(" · ");
            }
            words.push_str(&part);
        }
        (!words.is_empty()).then_some((words, tinted))
    }

    /// The trailing words: what asks for you, bold in its tone; else, faint,
    /// where a twin is and how many panes a split holds.
    fn trailing(&self, quiet: Color, disabled: bool, t: &Tokens) -> Option<El> {
        let f = face(self.mark, t);
        if self.mark != Mark::None && f.bold && !disabled {
            let word = f.word?;
            return Some(
                text(word)
                    .font(t.ty.caption.semibold())
                    .color(f.ink)
                    .a11y_hidden(),
            );
        }
        let split = (self.panes > 1).then(|| self.panes.to_string());
        let words: Vec<String> = [self.place.clone(), split].into_iter().flatten().collect();
        (!words.is_empty()).then(|| {
            text(words.join(" · "))
                .font(t.ty.caption)
                .color(quiet)
                .a11y_hidden()
        })
    }
}

/// The fold on a parent row: how many child rows it holds, and a chevron
/// that shows or hides them. Pointer only, as the session disclosure is; the
/// row's Left and Right fold it from the keyboard.
fn fold_toggle(pane: &Row, t: &Tokens) -> El {
    let (id, open) = (pane.id.clone(), pane.open);
    super::pressable(format!("fold-{}", pane.id), t)
        .h(t.space.s6)
        .px(t.space.s1)
        .gap(t.space.s0_5)
        .radius(t.radius.xs)
        .items_center()
        .focusable(false)
        .expanded(open)
        .label(format!(
            "{} {} child panes of {}",
            if open { "Hide" } else { "Show" },
            pane.children,
            pane.name
        ))
        .on_press(move |d: &mut Desk| {
            if !d.sidebar_unfolded.remove(&id) {
                d.sidebar_unfolded.insert(id.clone());
            }
        })
        .child(
            text(pane.children.to_string())
                .font(t.ty.caption)
                .color(t.roles.text.faint.paint())
                .a11y_hidden(),
        )
        .child(
            rune::icon(
                if open {
                    &icons::CHEVRON
                } else {
                    &icons::CHEVRON_RIGHT
                },
                t.icon.size.sm,
            )
            .color(t.roles.text.dim.paint())
            .a11y_hidden(),
        )
}

pub(super) fn pane_row(pane: &Row, t: &Tokens) -> El {
    let selected = pane.selected;
    let (target, session) = (pane.target.clone(), pane.session.to_owned());
    let sidebar = t.roles.layer.sidebar.paint();
    let host = host_color::on(pane.host.0, sidebar);
    let key = pane.key;
    let state = pane.state;
    let disabled = state == Forced::Disabled;
    let washed = selected || matches!(state, Forced::Hover | Forced::Press);
    // Faint never sits on a wash: a lit row promotes it to dim.
    let quiet = if washed || disabled {
        t.roles.text.dim
    } else {
        t.roles.text.faint
    };
    let said = pane.detail(t).map(|(words, _)| words).unwrap_or_default();
    let lead = tile::tile(pane.kind.or(pane.agent), pane.mark, sidebar, t);
    // The tile is named by its state's word where a screen reader meets
    // it, an idle one too, whose quiet tile has no badge to say so.
    let lead = match (pane.mark != Mark::None)
        .then(|| face(pane.mark, t).word)
        .flatten()
    {
        Some(word) => el().child(lead).label(word),
        None => lead,
    };
    let tip = if said.is_empty() {
        pane.name.clone()
    } else {
        format!("{}: {said}", pane.name)
    };
    let name_ink = if disabled {
        t.roles.text.faint
    } else {
        t.roles.text.text
    };
    let under = if selected {
        Base::Selected
    } else {
        Base::Plain
    };
    let base = forced_face(row().key(format!("pane-{}", pane.id)), under, state, t);
    let base = if state == Forced::Rest {
        t.kit.interactive(base)
    } else {
        base
    };
    let row = base
        .w_full()
        .h(t.space.row_h)
        .pl(t.space.s4 + t.space.s0_5 + if pane.child { t.space.s4 } else { 0.0 })
        .pr(t.space.s2)
        .gap(t.space.s2)
        .radius(t.radius.md)
        .continuous_corners()
        .items_center()
        .focusable(!disabled)
        .focus_selected(selected)
        .cursor(rune::Cursor::Pointer)
        .label(format!("{} in session {}", pane.name, pane.session))
        .description(said)
        .help(tip)
        .child(lead)
        .child(
            text(pane.name.clone())
                .font(t.ty.label.medium())
                .color(name_ink.paint())
                .truncate()
                .grow(1.0)
                .min_w(0.0)
                .a11y_hidden(),
        )
        .children(pane.trailing(quiet.paint(), disabled, t))
        .children((pane.children > 0).then(|| fold_toggle(pane, t)))
        .children(selected.then(|| host_color::stripe(host, t)));
    if disabled {
        return row;
    }
    let row = row.on_press_tx(move |d: &mut Desk, tx| {
        d.show(target.clone(), &session);
        return_to_terminal_on_click(tx);
    });
    let row = if pane.children > 0 {
        let (id, open) = (pane.id.clone(), pane.open);
        // A tree's Left folds and Right unfolds.
        row.expanded(open).on_key(move |d: &mut Desk, event, _| {
            let fold = match event.key {
                Key::Left => true,
                Key::Right => false,
                _ => return false,
            };
            if fold != open {
                return false;
            }
            if fold {
                d.sidebar_unfolded.remove(&id);
            } else {
                d.sidebar_unfolded.insert(id.clone());
            }
            true
        })
    } else {
        row
    };
    // A split moves as its tab, from the tab strip; a lone pane drags here.
    match key {
        Some(key) if pane.panes == 1 => drag::resource(row, pane.id, key),
        _ => row,
    }
}
