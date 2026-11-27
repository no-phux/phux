//! One ranked result, in the same anatomy on every row: a mark in a fixed
//! column (an agent's glyph and state for a terminal, a swatch for a
//! theme, a glyph for everything else), the title with its matched
//! characters lit, then on the trailing edge its detail or its keys,
//! what the row is up to (last, as the Sessions page says it), and a
//! check where you already are, in a slot every row of the list keeps.

use phux_protocol::ids::{ResourceId, ResourceKind};
use rune::{Cursor, El, Span, col, el, rich, row, text};

use super::super::sidebar::face;
use super::super::tile::{tells, tile};
use super::super::tokens::{Paint, Tokens};
use super::super::{icons, keycap};
use super::Grid;
use super::marks::{glyph, swatch};
use crate::activity::Mark;
use crate::addressable::{Addressable, Identity};
use crate::desk::Desk;
use crate::palette::{Hit, Item, Target};
use crate::ui::tokens::Voice;

/// What every row of one palette draw shares.
pub(super) struct Rows<'a> {
    pub desk: &'a Desk,
    pub t: &'a Tokens,
    pub grid: Grid,
    /// The keyboard's row: the one Return runs.
    pub picked: usize,
    /// Whether the picked row is washed. A phone's touch list shows none
    /// until a query makes its first row the one Return runs.
    pub wash: bool,
    /// The fill under the rows: a status badge is ringed in it.
    pub ground: rune::Color,
    /// Theme rows drop their `Theme:` prefix under a Themes heading or in
    /// the theme picker.
    pub bare_themes: bool,
    pub shown: Vec<Addressable>,
    pub current_terminal: Option<ResourceId>,
    pub current_session: Option<String>,
    pub kept_theme: Option<&'static str>,
    /// Whether any row of the list can be where you are: then every row
    /// keeps the check's slot, so their trailing edges line up.
    pub checks: bool,
}

impl Rows<'_> {
    /// The row for `hit`, the `index`th of the list.
    pub(super) fn row(&self, hit: &Hit, index: usize) -> El {
        let (s, r) = (&self.t.space, &self.t.roles);
        let g = &self.grid;
        let picked = self.picked == index;
        let washed = picked && self.wash;
        let base = row()
            .key(format!("palette-row-{index}"))
            .role("option")
            .label(hit.item.title.clone())
            .description(self.spoken_detail(&hit.item))
            .w_full()
            .min_h(g.row_h)
            // A phone's two-line rows breathe as the Sessions page's do.
            .py(if g.phone { s.s2 } else { s.s1 })
            .px(g.pad)
            .gap(g.gap)
            .radius(g.row_radius(self.t))
            .items_center()
            .focusable(true)
            .focus_selected(picked);
        // A tab's terminals sit under its heading: their marks in its
        // title's column.
        let base = if hit.item.kind == "pane" {
            base.pl(g.pad + g.slot + g.gap)
        } else {
            base
        };
        let base = if washed {
            base.fill(r.wash.current.paint())
        } else {
            base
        };
        let target = hit.item.target.clone();
        let base = self
            .t
            .kit
            .interactive(base)
            .cursor(Cursor::Pointer)
            .on_press_tx(move |d: &mut Desk, tx| d.palette_run_target(target.clone(), tx))
            .child(g.mark_cell(format!("palette-mark-{index}"), self.mark(hit, washed)));
        if g.phone {
            return base
                .child(self.lines(hit, index))
                .children(self.check_slot(&hit.item, index));
        }
        base.child(self.title(hit, index))
            .child(el().grow(1.0))
            .child(self.trailing(hit, index, washed))
    }

    /// The title, then on a phone what a desktop puts on the trailing
    /// edge, as a second line in the Sessions page's grammar.
    fn lines(&self, hit: &Hit, index: usize) -> El {
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(self.t.space.s0_5)
            .child(self.title(hit, index))
            .children(self.subtitle(&hit.item))
    }

    /// A phone row's second line: its detail, then its state in its tone,
    /// in the Sessions page's order; one text that wraps at large sizes
    /// rather than running off the card.
    fn subtitle(&self, item: &Item) -> Option<El> {
        let status = self.status(item);
        let words: Vec<&str> = [Some(item.detail.as_str()), status.map(|(word, _)| word)]
            .into_iter()
            .flatten()
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            return None;
        }
        let line = words.join(" · ");
        let spans = status
            .map(|(word, ink)| vec![Span::new(line.len() - word.len(), line.len()).color(ink)])
            .unwrap_or_default();
        Some(
            rich(line, spans)
                .font(self.grid.ty.caption)
                .color(self.t.roles.text.dim.paint())
                .wrap()
                .w_full()
                .a11y_hidden(),
        )
    }

    fn title(&self, hit: &Hit, index: usize) -> El {
        let r = &self.t.roles;
        let (title, matched) = self.shown_title(hit);
        let heavy = self.grid.ty.body.semibold().weight;
        // A grey accent (the house look's, or one that fell back to body
        // text, `Roles::emphasis`) cannot mark a match by its colour: an
        // underline does, beside the heavier weight.
        let underline = grey(r.accent.accent_text).then(|| r.accent.accent_text.paint());
        let spans = matched
            .iter()
            .filter_map(|&at| title.char_indices().nth(at))
            .map(|(start, ch)| {
                let mut span = Span::new(start, start + ch.len_utf8())
                    .color(r.accent.accent_text.paint())
                    .weight(heavy);
                span.underline = underline;
                span
            })
            .collect();
        let title = rich(title, spans)
            .key(format!("palette-title-{index}"))
            .font(self.grid.ty.body)
            .color(r.text.text.paint())
            .min_w(0.0)
            .shrink()
            .a11y_hidden();
        // A phone's large text wraps a title whole rather than cut it.
        if self.grid.phone {
            title.wrap().w_full()
        } else {
            title.truncate()
        }
    }

    /// The title as drawn, and its matched characters: a theme under a
    /// Themes heading is named without the prefix that heading says.
    fn shown_title(&self, hit: &Hit) -> (String, Vec<usize>) {
        const PREFIX: &str = "Theme: ";
        let title = &hit.item.title;
        let bare = matches!(hit.item.target, Target::Theme(_)) && self.bare_themes;
        match title.strip_prefix(PREFIX).filter(|_| bare) {
            Some(rest) => {
                let skip = PREFIX.chars().count();
                let matched = hit.matched.iter().filter_map(|&at| at.checked_sub(skip));
                (rest.to_owned(), matched.collect())
            }
            None => (title.clone(), hit.matched.clone()),
        }
    }

    /// Where the row is (keys for a command), what it is up to, then the
    /// check's slot.
    fn trailing(&self, hit: &Hit, index: usize, washed: bool) -> El {
        let s = &self.t.space;
        // Faint never sits on a wash: the picked row's detail reads dim.
        let ink = if washed {
            self.t.roles.text.dim
        } else {
            self.t.roles.text.faint
        };
        let host = match &hit.item.target {
            Target::Screen(host, _) => Some(host.as_deref().unwrap_or(crate::machines::LOCAL)),
            _ => None,
        };
        let status = self.status(&hit.item);
        let detail = &hit.item.detail;
        row()
            .items_center()
            .min_w(0.0)
            .shrink()
            .gap(s.icon_gap)
            .children(
                host.map(|host| {
                    crate::host_color::dot(host, s.s2, self.t.roles.layer.overlay.paint())
                }),
            )
            .children((!detail.is_empty()).then(|| self.detail(&hit.item, ink.paint())))
            .children(
                (status.is_some() && !detail.is_empty()).then(|| self.words("·", ink.paint())),
            )
            .children(status.map(|(word, ink)| {
                text(word)
                    .font(self.grid.ty.caption.medium())
                    .color(ink)
                    .a11y_hidden()
            }))
            .children(self.check_slot(&hit.item, index))
    }

    /// The check where you are, in a slot every row of a list that can
    /// hold one keeps, empty or not.
    fn check_slot(&self, item: &Item, index: usize) -> Option<El> {
        let size = self.t.icon.size.md;
        let slot = el().key(format!("palette-check-{index}")).w(size).center();
        if self.current(item) {
            return Some(
                slot.child(
                    rune::icon(&icons::CHECK, size)
                        .color(self.t.roles.accent.accent_text.paint())
                        .a11y_hidden(),
                ),
            );
        }
        self.checks.then_some(slot.a11y_hidden())
    }

    fn words(&self, words: &str, ink: rune::Color) -> El {
        text(words.to_owned())
            .font(self.grid.ty.caption)
            .color(ink)
            .a11y_hidden()
    }

    /// A command's keys as a key cap (`⇧⌘P`), like every shortcut Rune
    /// shows; any other detail (a session, a pane id, what the row does)
    /// as words.
    fn detail(&self, item: &Item, ink: rune::Color) -> El {
        if details_keys(item) {
            return keycap(item.detail.clone(), self.t);
        }
        detail_text(&item.detail, ink, &self.grid.ty)
    }

    /// A terminal's state worth telling, as its word in its tone's ink.
    fn status(&self, item: &Item) -> Option<(&'static str, rune::Color)> {
        let id = terminal_id(item)?;
        let mark = self.desk.activities.mark(id);
        if !tells(mark) {
            return None;
        }
        let f = face(mark, self.t);
        let word = f.word.unwrap_or_else(|| mark.word());
        (!word.is_empty()).then_some((word, f.ink))
    }

    /// The detail a screen reader hears, in the order it is drawn.
    fn spoken_detail(&self, item: &Item) -> String {
        let status = self.status(item).map(|(word, _)| word);
        let current = self.current(item).then_some("current");
        let detail = (!item.detail.is_empty()).then_some(item.detail.as_str());
        let parts: Vec<&str> = [detail, status, current].into_iter().flatten().collect();
        parts.join(" · ")
    }

    /// Whether the row is where you already are: the terminal on screen,
    /// its session, the theme Escape keeps.
    fn current(&self, item: &Item) -> bool {
        match &item.target {
            Target::Terminal { id, .. } if item.kind != "tab" => {
                self.current_terminal.as_ref() == Some(id)
            }
            Target::Session(name) => self.current_session.as_deref() == Some(name),
            Target::Theme(id) => *id == self.kept_theme,
            _ => false,
        }
    }

    /// What a row is: a terminal's identity and state, a theme's swatch,
    /// or the glyph for what it does.
    fn mark(&self, hit: &Hit, washed: bool) -> El {
        let t = self.t;
        let size = self.mark_size();
        let ink = if washed {
            t.roles.text.text
        } else {
            t.roles.text.dim
        };
        match &hit.item.target {
            Target::Theme(id) => swatch(*id, size, t),
            Target::Terminal { id, .. } => self.identity(&hit.item, id),
            target => rune::icon(glyph(target, &hit.item), size)
                .color(ink.paint())
                .a11y_hidden(),
        }
    }

    /// A glyph's size: the icon tokens already grow with the text.
    fn mark_size(&self) -> f32 {
        self.t.icon.size.md
    }

    /// A terminal's tile, as the sidebar and the Sessions page draw it: its
    /// agent's glyph, its state's badge, and a tab's stacked edges.
    fn identity(&self, item: &Item, id: &ResourceId) -> El {
        let resource = self.shown.iter().find(|r| &r.id == id);
        let identity = resource.map_or_else(
            || Identity::kind(ResourceKind::Terminal),
            |r| r.presentation(self.desk).identity,
        );
        let tab = item.kind == "tab";
        let mark = if tab {
            Mark::None
        } else {
            self.desk.activities.mark(id)
        };
        tile(
            &identity,
            mark,
            if tab { 2 } else { 1 },
            self.ground,
            self.t,
        )
    }
}

/// Whether `ink` reads as a grey, whose hue cannot set a match apart.
fn grey(ink: phux_design::Rgba8) -> bool {
    /// The widest channel spread (of 255) still read as grey.
    const SPREAD: u8 = 24;
    ink.r.max(ink.g).max(ink.b) - ink.r.min(ink.g).min(ink.b) <= SPREAD
}

/// Whether a row can be where you are: a terminal, a session or a theme.
pub(super) fn can_be_current(item: &Item) -> bool {
    match &item.target {
        Target::Terminal { .. } => item.kind != "tab",
        Target::Session(_) | Target::Theme(_) => true,
        _ => false,
    }
}

fn terminal_id(item: &Item) -> Option<&ResourceId> {
    match &item.target {
        Target::Terminal { id, .. } if item.kind != "tab" => Some(id),
        _ => None,
    }
}

/// Whether an item's detail is its command's chords, not words about it.
pub(super) fn details_keys(item: &Item) -> bool {
    let Target::Action(action) = item.target else {
        return false;
    };
    let detail = &item.detail;
    !detail.is_empty()
        && crate::keys::command(action).is_some_and(|c| *detail == c.hint() || *detail == c.hints())
}

/// A row's detail in its voices: the session's name in the ui face,
/// a pane id (`@N`) after it as a machine noun, mono with tabular figures.
fn detail_text(detail: &str, ink: rune::Color, ty: &crate::ui::tokens::Type) -> El {
    let words = |w: String, font| text(w).font(font).color(ink).truncate();
    let id = detail.rsplit_once(" · ").filter(|(_, id)| {
        id.strip_prefix('@')
            .is_some_and(|n| n.parse::<u32>().is_ok())
    });
    match id {
        Some((name, id)) => row()
            .items_center()
            .min_w(0.0)
            .child(words(format!("{name} · "), ty.caption).shrink().min_w(0.0))
            .child(words(id.to_owned(), ty.readout))
            .a11y_hidden(),
        None => words(detail.to_owned(), ty.caption)
            .shrink()
            .min_w(0.0)
            .a11y_hidden(),
    }
}
