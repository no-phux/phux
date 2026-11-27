//! A sidebar row: one tab with inline members, or a resource no tab holds.

use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Color, El, Key, col, el, row, text};

use super::super::rows::{Base, State as Forced, forced_face};
use super::super::{Paint, Tokens, tile};
use super::{drag, face, return_to_terminal_on_click};
use crate::activity::Mark;
use crate::addressable::Identity;
use crate::desk::Desk;
use crate::host_color;
use crate::ui::tokens::Voice;

/// A row: one tab of its session, or a resource no tab holds, with its session
/// kept separately actionable. It reads as the work it
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
    pub(super) identity: Identity,
    /// The loudest of its panes' marks.
    pub(super) mark: Mark,
    /// How many panes the tab splits into.
    pub(super) panes: usize,
    pub(super) selected: bool,
    /// Actual tab members, for the inline second line. Empty for singles.
    pub(super) members: Vec<Member>,
    /// The terminal's machine and its colour, so a recolour redraws.
    pub(super) host: (&'a str, u32),
    /// The session's colour, for its quiet mark.
    pub(super) color: Option<u32>,
    /// A look forced for the gallery's states page; `Rest` is the live row.
    pub(super) state: Forced,
}

#[derive(Hash)]
pub(super) struct Member {
    pub(super) id: ResourceId,
    pub(super) name: String,
    pub(super) caption: String,
    pub(super) selected: bool,
    pub(super) mark: Mark,
}

impl Row<'_> {
    /// The existing identity tile, with its spoken state kept beside the row.
    fn identity(&self, ground: Color, t: &Tokens) -> El {
        let lead = tile::tile(&self.identity, self.mark, self.panes, ground, t);
        match (self.mark != Mark::None)
            .then(|| face(self.mark, t).word)
            .flatten()
        {
            Some(word) => el().child(lead).label(word),
            None => lead,
        }
    }

    /// Useful detail remains readable at rest; disabled text stays quiet.
    fn inks(&self, t: &Tokens) -> (Color, Color) {
        let quiet = if self.state == Forced::Disabled {
            t.roles.text.faint
        } else {
            t.roles.text.dim
        };
        let name = if self.state == Forced::Disabled {
            t.roles.text.faint
        } else {
            t.roles.text.text
        };
        (quiet.paint(), name.paint())
    }

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
        let split = (self.panes > 1).then(|| {
            format!(
                "{} panes: {}",
                self.panes,
                self.members
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
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
    fn trailing(&self, quiet: Color, disabled: bool, context: bool, t: &Tokens) -> Option<El> {
        let f = face(self.mark, t);
        let label = if self.mark != Mark::None && f.bold && !disabled {
            f.word.map(|word| {
                text(word)
                    .font(t.ty.caption.semibold())
                    .color(f.ink)
                    .a11y_hidden()
            })
        } else {
            (!context)
                .then_some(self.place.as_ref())
                .flatten()
                .map(|place| {
                    text(place.clone())
                        .font(t.ty.caption)
                        .color(quiet)
                        .max_w(t.space.s16)
                        .min_w(0.0)
                        .truncate()
                        .a11y_hidden()
                })
        };
        let count = (self.panes > 1).then(|| {
            text(self.panes.to_string())
                .font(t.ty.caption)
                .color(quiet)
                .a11y_hidden()
        });
        if label.is_none() && count.is_none() {
            return None;
        }
        // The exact count is independent of elided detail, including MIME and
        // read-only mirror labels. A narrow row must never truncate it away.
        Some(
            row()
                .gap(t.space.s1)
                .items_center()
                .children(label)
                .children(count),
        )
    }
}

/// Real split members are directly selectable, not a hidden disclosure. The
/// row owns keyboard focus; Left/Right reach members without another Tab stop.
fn member(member: &Member, session: &str, disabled: bool, t: &Tokens) -> El {
    let (id, session) = (member.id.clone(), session.to_owned());
    let word = face(member.mark, t).word.unwrap_or_default();
    let base = super::super::pressable(format!("sidebar-member-{}", member.id), t)
        .min_h(t.space.s4)
        .min_w(0.0)
        .shrink()
        .focusable(false)
        .label(format!("{} · {word}", member.name))
        .help(format!("Open {}", member.name))
        .child(
            text(member.caption.clone())
                .font(t.ty.caption)
                .color(
                    if member.selected {
                        t.roles.text.text
                    } else {
                        t.roles.text.dim
                    }
                    .paint(),
                )
                .truncate(),
        );
    // Forced colours can give current and neighbouring captions identical ink.
    // Mark the actual shown member without changing its pitch or hit bounds.
    let base = if member.selected {
        base.description_static("Current pane").child(
            el().key("sidebar-current-member")
                .pin(Some(0.0), None, Some(0.0), Some(0.0))
                .h(t.space.hairline)
                .fill(t.roles.text.text.paint())
                .inert()
                .a11y_hidden(),
        )
    } else {
        base
    };
    if disabled {
        base.inert()
    } else {
        base.on_press_tx(move |d: &mut Desk, tx| {
            d.show(id.clone(), &session);
            return_to_terminal_on_click(tx);
        })
    }
}

fn name(
    pane: &Row,
    ink: Color,
    quiet: Color,
    disabled: bool,
    session: Option<&super::Header>,
    t: &Tokens,
) -> El {
    let title = text(pane.name.clone())
        .font(t.ty.label.medium())
        .color(ink)
        .truncate()
        .a11y_hidden();
    let top = row()
        .w_full()
        .min_w(0.0)
        .gap(t.space.icon_gap)
        .items_center()
        .child(title.grow(1.0).min_w(0.0))
        .children(pane.trailing(quiet, disabled, session.is_some(), t));
    let base = col().grow(1.0).min_w(0.0).child(top);
    if let Some(session) = session {
        return base.child(
            row()
                .w_full()
                .min_w(0.0)
                .gap(t.space.s1)
                .items_center()
                .child(
                    super::session_heading(session, t)
                        .shrink()
                        .min_w(t.space.pointer_min)
                        .h(t.space.pointer_min),
                )
                .children(pane.place.as_ref().map(|place| {
                    text(format!("· {place}"))
                        .font(t.ty.caption)
                        .color(quiet)
                        .truncate()
                        .min_w(0.0)
                        .a11y_hidden()
                })),
        );
    }
    if pane.members.is_empty() {
        return base;
    }
    let mut members = row().w_full().min_w(0.0).gap(t.space.s0_5).items_center();
    for (index, item) in pane.members.iter().enumerate() {
        if index > 0 {
            members = members.child(
                text("·")
                    .font(t.ty.caption)
                    .color(t.roles.text.dim.paint())
                    .a11y_hidden(),
            );
        }
        members = members.child(member(item, pane.session, disabled, t));
    }
    base.child(members)
}

pub(super) fn pane_row(pane: &Row, t: &Tokens) -> El {
    render_row(pane, None, t)
}

/// One flat resource row, with a separately owned session handle in its quiet
/// context line. The whole row still accepts drops on the session.
pub(super) fn single_row(pane: &Row, session: &super::Header, t: &Tokens) -> El {
    let body = el()
        .key(format!("session-{}", session.id))
        .w_full()
        .child(render_row(pane, Some(session), t));
    drag::reorderable(body, session.key, session.target, t)
}

fn render_row(pane: &Row, session: Option<&super::Header>, t: &Tokens) -> El {
    let selected = pane.selected;
    let sidebar = t.roles.layer.sidebar.paint();
    let host = host_color::on(pane.host.0, sidebar);
    let state = pane.state;
    let disabled = state == Forced::Disabled;
    let (quiet, name_ink) = pane.inks(t);
    let said = pane.detail(t).map(|(words, _)| words).unwrap_or_default();
    let tip = if said.is_empty() {
        pane.name.clone()
    } else {
        format!("{}: {said}", pane.name)
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
        .min_h(t.space.row_h)
        .py(t.space.s0_5)
        .pl(t.space.s1)
        .pr(t.space.s2)
        .gap(t.space.icon_gap)
        .radius(t.radius.md)
        .continuous_corners()
        .items_center()
        .hover_group()
        .focusable(!disabled)
        .focus_selected(selected)
        .cursor(rune::Cursor::Pointer)
        .label(format!("{} in session {}", pane.name, pane.session))
        .description(said)
        .help(tip)
        .child(pane.identity(sidebar, t))
        .child(name(pane, name_ink, quiet, disabled, session, t))
        .children(session.map(|session| {
            super::super::reveal(
                super::actions::session_controls(session.key, session.name, t),
                true,
            )
        }))
        .children(selected.then(|| host_color::stripe(host, t)));
    row_actions(row, pane)
}

/// Keep pointer selection, remembered split targets and drag ownership.
fn row_actions(row: El, pane: &Row) -> El {
    if pane.state == Forced::Disabled {
        return row;
    }
    let (target, session) = (pane.target.clone(), pane.session.to_owned());
    let row = row.on_press_tx(move |d: &mut Desk, tx| {
        d.show(target.clone(), &session);
        return_to_terminal_on_click(tx);
    });
    let members: Vec<_> = pane
        .members
        .iter()
        .map(|m| (m.id.clone(), m.selected))
        .collect();
    let session = pane.session.to_owned();
    let row = row.on_key(move |d: &mut Desk, event, _| {
        let next = match event.key {
            Key::Left => false,
            Key::Right => true,
            _ => return false,
        };
        if members.is_empty() {
            return true;
        }
        let at = members
            .iter()
            .position(|(_, selected)| *selected)
            .unwrap_or(0);
        let index = if next {
            (at + 1) % members.len()
        } else {
            (at + members.len() - 1) % members.len()
        };
        d.show(members[index].0.clone(), &session);
        true
    });
    // A split moves as its tab, from the tab strip; a lone pane drags here.
    match pane.key {
        Some(key) if pane.panes == 1 => drag::resource(row, pane.id, key),
        Some(key) => drag::resource_target(row, pane.id, key),
        None => row,
    }
}
