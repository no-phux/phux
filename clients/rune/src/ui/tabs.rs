//! The tab strip, shown once a session has more than one tab, and the
//! rename dialog. Each tab leads with the tile its sidebar row has.
//!
//! Tabs are phux's own (OWN), on tokens: kit `TabStrip` draws the
//! active tab as a pressed overlay with no lifted edge or elevation, sits
//! at a fixed 28 points, colours busy dots with the accent and keys its
//! tabs itself, where phux keeps `tab-N`, `Grab::Tab` and the menus.

use crate::addressable::Addressable;
use crate::ui::tokens::Voice;
use crate::ui::tokens::layout::TABS;
use phux_design::tokens::{SPACE, Space};
use phux_protocol::ids::ResourceId;
use rune::{El, row, text, text_input};
use rune_kit::{Button, ButtonVariant, Dialog};
use std::collections::HashMap;

use super::controls::{return_to_terminal_on_click, terminal_icon_press};
use super::rows::{Base, State as Forced, forced_face};
use super::sidebar::face;
use super::tokens::Tier;
use super::{Cover, Paint, Tokens, icons, reveal, tip};
use crate::activity::Mark;
use crate::desk::{Desk, Grab, Renaming, Tab};
use crate::keys::{self, Shortcut};

/// How tabs share the bar, in points on the 4 point grid.
#[derive(Debug, Clone, Copy)]
struct Fit {
    /// The widest a tab grows, and the least it keeps: the start of its name.
    widest: f32,
    narrowest: f32,
    /// The active tab stays readable however many crowd it.
    active_least: f32,
    /// The gap a divider stands in, and the new-tab button after the strip.
    between: f32,
    after: f32,
    /// Narrower than this, a resting tab is its name alone: no chord, and
    /// no close button waiting under the pointer for room the name needs.
    roomy: f32,
}

impl Fit {
    const fn of(s: &Space) -> Self {
        Self {
            widest: TABS.widest,
            narrowest: s.s8,
            active_least: TABS.active_least,
            between: s.s1,
            after: s.pointer_min + s.s1,
            roomy: TABS.roomy,
        }
    }

    /// A resting tab's share of `room` points among `count`, never past the
    /// widest. The active tab keeps its least first, so a crowd squeezes
    /// the others. A bar not laid out yet has no room to report and gives
    /// every tab its fill.
    #[allow(clippy::cast_precision_loss, reason = "a strip holds few tabs")]
    fn share(self, room: f32, count: usize) -> f32 {
        if room <= 0.0 || count == 0 {
            return self.widest;
        }
        let rest = count.saturating_sub(1) as f32;
        let free = room - self.after - self.between * rest;
        let even = free / count as f32;
        if even >= self.active_least || rest == 0.0 {
            return even.min(self.widest);
        }
        ((free - self.active_least) / rest).clamp(self.narrowest, self.widest)
    }

    /// Whether `count` tabs at their least still overrun `room`.
    #[allow(clippy::cast_precision_loss, reason = "a strip holds few tabs")]
    fn crowded(self, room: f32, count: usize) -> bool {
        let rest = count.saturating_sub(1) as f32;
        room > 0.0 && self.active_least + rest * (self.narrowest + self.between) + self.after > room
    }
}

const FIT: Fit = Fit::of(&SPACE);

/// The chord that reaches the tab at `index`, where the platform writes
/// chords short enough to sit in a tab (⌘1; `Ctrl+Shift+1` waits in the
/// tooltip).
fn chord(index: usize) -> Option<String> {
    let n = u8::try_from(index).ok().filter(|i| *i < 9)?;
    let hint = keys::command(Shortcut::Tab(n))?.hint();
    (hint.chars().count() <= 3).then_some(hint)
}

/// The tab's trailing slot: its chord at rest and its close button under the
/// pointer, in the same place so neither moves the name. The active tab
/// always shows its close.
fn trailing(index: usize, tab: &Tab, width: f32, t: &Tokens) -> Option<El> {
    let active = tab.active;
    if !active && width < FIT.roomy {
        return None;
    }
    let slot = t.icon.button.sm;
    let dim = t.roles.text.dim.paint();
    // A locked tab has no close to offer: its lock sits where close would.
    if tab.locked {
        let lock = rune::icon(&icons::LOCK, t.icon.size.xs)
            .color(dim)
            .a11y_hidden();
        let lock = rune::el().size(slot, slot).center().child(lock);
        return Some(lock.help("Locked: unlock it to close it"));
    }
    let close = rune_kit::IconButton::new(&icons::CLOSE)
        .variant(rune_kit::IconButtonVariant::Ghost)
        .control_size(rune_kit::ControlSize::Small)
        .label("Close tab")
        .on_press(move |d: &mut Desk| d.close_tab(index))
        .build(&t.kit)
        .key(format!("tab-close-{index}"))
        .max_w(slot)
        .max_h(slot)
        // Pointer only: the strip's arrows select what they reach, and ⌘W
        // closes the tab from the keyboard.
        .focusable(false);
    let close = tip(close, "Close tab", Shortcut::CloseTab);
    if active {
        return Some(close);
    }
    let hint = chord(index).map(|hint| {
        reveal(
            text(hint)
                .font(t.ty.caption)
                .color(t.roles.text.faint.paint()),
            false,
        )
        .a11y_hidden()
    });
    Some(
        row()
            .h(slot)
            .min_w(slot)
            .items_center()
            .justify_end()
            .children(hint)
            .child(reveal(close.pin(None, Some(0.0), Some(0.0), None), true)),
    )
}

/// The loudest state among a tab's terminals that its tile's badge tells.
fn loudest(desk: &Desk, tab: &Tab) -> Option<Mark> {
    super::tile::loudest(tab.resources.iter().map(|id| desk.activities.mark(id)))
}

/// The tab's tile: the agent working in it, the first one of a split, or
/// the prompt; badged with its loudest state. The sidebar's rows lead with
/// the same tile.
fn lead(desk: &Desk, tab: &Tab, all: &HashMap<&ResourceId, &Addressable>, t: &Tokens) -> El {
    let members: Vec<_> = tab
        .resources
        .iter()
        .filter_map(|id| all.get(id).copied())
        .collect();
    let lead = members
        .iter()
        .find(|r| desk.activities.agent_name(&r.id).is_some())
        .or(members.first());
    let identity = lead
        .map(|r| r.presentation(desk).identity)
        .unwrap_or_else(|| {
            crate::addressable::Identity::kind(phux_protocol::ids::ResourceKind::Unknown { tag: 0 })
        });
    let mark = loudest(desk, tab).unwrap_or(Mark::None);
    super::tile::tile(
        &identity,
        mark,
        members.len(),
        t.roles.layer.ground.paint(),
        t,
    )
}

/// The glyph is already in the identity surface. Keep the tab's authoritative
/// given/remembered name, without changing the palette/window-title contract.
fn navigation_label(desk: &Desk, tab: &Tab, all: &HashMap<&ResourceId, &Addressable>) -> String {
    if !tab.name.is_empty() && tab.name.parse::<u32>().is_err() {
        return tab.name.clone();
    }
    desk.tab_focus(tab)
        .or(tab.resources.first())
        .and_then(|id| all.get(id))
        .map_or_else(|| desk.tab_label(tab), |r| r.presentation(desk).name)
}

/// A tab's face for `state` over its selection (`rows::forced_face`).
fn face_for(base: El, active: bool, state: Forced, t: &Tokens) -> El {
    let under = if active { Base::Selected } else { Base::Plain };
    let face = forced_face(base, under, state, t);
    if state == Forced::Rest {
        t.kit.interactive(face)
    } else {
        face
    }
}

/// What a live tab answers to: select, rename, its menu and a drag.
fn handled(base: El, index: usize, grab: Grab) -> El {
    base.on_press_tx(move |d: &mut Desk, tx| {
        d.click_tab(index);
        return_to_terminal_on_click(tx);
    })
        // Each press of a double click ran `click_tab`; the tab renamed
        // is the one shown.
        .on_double_press(move |d: &mut Desk| {
            d.select_tab(index);
            d.rename_tab(index);
        })
        .on_secondary_press_local(move |d: &mut Desk, event, _| {
            d.open_tab_menu(index, event.window());
        })
        .on_drag(move |d: &mut Desk, drag, _| d.drag_reorder(grab, drag))
        .on_layout_tx(move |d: &mut Desk, rect, tx| {
            d.connection.reorder.laid_out(grab, rect);
            tx.live_only();
        })
}

fn tab_name(label: &str, active: bool, state: Forced, t: &Tokens) -> El {
    let ink = match state {
        Forced::Disabled => t.roles.text.faint,
        _ if active || state == Forced::Hover => t.roles.text.text,
        _ => t.roles.text.dim,
    };
    let name = text(label.to_owned())
        .font(t.ty.label.medium())
        .color(ink.paint())
        // A long name fades out, as a browser's does: no ellipsis taking
        // letters from a narrow tab.
        .truncate_fade_width(1.2)
        .a11y_hidden();
    if active {
        name
    } else {
        name.group_color(t.roles.text.text.paint())
    }
}

fn tab(
    desk: &Desk,
    tab: &Tab,
    index: usize,
    width: f32,
    t: &Tokens,
    state: Forced,
    all: &HashMap<&ResourceId, &Addressable>,
) -> El {
    let s = &t.space;
    let loud = loudest(desk, tab);
    let word = loud.and_then(|m| face(m, t).word);
    let label = navigation_label(desk, tab, all);
    let name = tab_name(&label, tab.active, state, t);
    let roomy = width >= FIT.roomy;
    let grab = Grab::Tab(tab.id);
    let target = desk.connection.reorder.target() == Some(grab);
    let base = rune::el()
        .key(format!("tab-{index}"))
        .cursor(rune::Cursor::Pointer);
    let base = face_for(base, tab.active, state, t)
        .h(s.row_h)
        .pl(if roomy { s.s2 + s.s0_5 } else { s.icon_gap })
        .pr(if roomy || tab.active { s.icon_gap } else { s.s1 })
        .gap(s.icon_gap)
        .radius(t.radius.sm)
        .continuous_corners()
        .items_center()
        .hover_group()
        .role("tab")
        .label(label.clone())
        .description(word.unwrap_or_default())
        .focusable(state != Forced::Disabled)
        .focus_selected(tab.active)
        // The selection moves between tabs on the select spring.
        .token_motion(t.motion.select.enter);
    let base = if state == Forced::Disabled {
        base
    } else {
        handled(base, index, grab)
    };
    let base = if tab.active {
        // The one you are on never gives up its name to a crowd. It is
        // The selected treatment (`face_for`), lifted onto the raised
        // tier.
        t.elevate(base, Tier::Raised)
            .max_w(width.max(FIT.active_least))
    } else {
        // `shrink` clears the floor, so the floor is set after it.
        base.max_w(width).shrink().min_w(FIT.narrowest.min(width))
    };
    // Where a dragged tab would land.
    let base = if target {
        base.border(s.hairline, t.roles.border.focus.paint())
    } else {
        base
    };
    let n = u8::try_from(index).ok().filter(|i| *i < 9);
    let base = match n {
        Some(n) => tip(base, label, Shortcut::Tab(n)),
        None => base.help(label),
    };
    base.child(lead(desk, tab, all, t))
        .child(name)
        .child(rune::el().grow(1.0))
        .children(trailing(index, tab, width, t))
}

/// The hairline between two resting tabs. Beside the active tab it keeps its
/// room and draws nothing, so the row never shifts as the tab moves.
fn divider(shown: bool, t: &Tokens) -> El {
    let line = rune::el().w(t.space.hairline).h(t.space.s3);
    rune::el()
        .w(FIT.between)
        .h(t.space.row_h)
        .center()
        .a11y_hidden()
        .child(if shown {
            line.fill(t.roles.border.muted.paint())
        } else {
            line
        })
}

pub fn strip(desk: &Desk, ink: &Tokens) -> Option<El> {
    let t = ink;
    let tabs = &desk.connection.tabs.list;
    if tabs.len() < 2 {
        return None;
    }
    let room = desk.connection.tabs.room;
    let resources = desk.addressable();
    let all = resources.iter().map(|r| (&r.id, r)).collect();
    let width = FIT.share(room, tabs.len());
    // One Tab stop; the arrows move among the tabs and select as they go.
    let mut line = row()
        .key("tabs")
        .role("tablist")
        .label("Tabs")
        .focus_group()
        .shrink()
        .min_w(0.0)
        .items_center();
    if FIT.crowded(room, tabs.len()) {
        // More tabs than the bar holds fade out at its end, never over it.
        line = line.clip_fade(0.0, 0.0, t.space.s4, 0.0);
    }
    for (index, item) in tabs.iter().enumerate() {
        if index > 0 {
            let beside_active = item.active || tabs[index - 1].active;
            line = line.child(divider(!beside_active, t));
        }
        line = line.child(tab(desk, item, index, width, t, Forced::Rest, &all));
    }
    let add = terminal_icon_press(&icons::PLUS, "New tab", "tab-new", t, Desk::new_tab);
    Some(
        row()
            .shrink()
            .min_w(0.0)
            .gap(t.space.s1)
            .items_center()
            .child(line)
            .child(tip(add, "New tab", Shortcut::NewTerminal)),
    )
}

/// The rename field: the kit field's look (the control outline,
/// `radius.sm`) around Rune's own editor, which keeps the `rename-field`
/// key the native input routes by.
fn rename_field(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    row()
        .w_full()
        .min_h(s.row_h)
        .px(s.s2)
        .py(s.s1)
        .radius(t.radius.sm)
        .items_center()
        .fill(t.roles.layer.ground.paint())
        .border(s.hairline, t.roles.border.control.paint())
        .child(
            text_input(&desk.connection.tabs.rename_field, t.ty.body)
                .key("rename-field")
                .label("New name")
                .single_line()
                .grow(1.0)
                .color(t.roles.text.text.paint())
                .on_edit(|d: &mut Desk| &mut d.connection.tabs.rename_field)
                .on_input_key(|d: &mut Desk, event, tx| d.rename_key(event, tx))
                .on_submit(|d: &mut Desk, tx| d.submit_rename(tx)),
        )
}

/// Renaming a tab or session: a short question, so the kit's fitted dialog.
pub fn rename_sheet(desk: &Desk, ink: &Tokens) -> Option<El> {
    let t = ink;
    let s = &t.space;
    let title = match desk.connection.tabs.renaming.as_ref()? {
        Renaming::Session(_) => "Rename session",
        Renaming::Tab(_) => "Rename tab",
    };
    // A phone has no Return or Esc to name, and takes a thumb.
    if desk.phone.is_some() {
        return Some(phone_rename(desk, title, t));
    }
    let save = save_button("Save (Return)", t);
    let cancel = Button::new("Cancel (Esc)")
        .variant(ButtonVariant::Outline)
        .on_press(Desk::cancel_rename)
        .build(&t.kit)
        .key("rename-cancel")
        .label("Cancel");
    let foot = row()
        .w_full()
        .gap(s.s2)
        .items_center()
        .child(rune::el().grow(1.0))
        .child(cancel)
        .child(save);
    let body = rune::col()
        .w_full()
        .px(s.s4)
        .pb(s.s4)
        .gap(s.s3)
        .child(rename_field(desk, t))
        .child(foot);
    let mut layer = Vec::new();
    Dialog::new("rename", title)
        .open(true)
        .fit()
        .width(TABS.rename_w)
        .on_close(Desk::cancel_rename)
        .child(body)
        .build(&t.kit, &mut layer);
    Some(rune::el().cover().children(layer))
}

/// Save: the dialog's default button, Return's.
fn save_button(words: &'static str, t: &Tokens) -> El {
    Button::new(words)
        .variant(ButtonVariant::Submit)
        .on_press_tx(|d: &mut Desk, tx| d.submit_rename(tx))
        .build(&t.kit)
        .key("rename-save")
        .label("Save")
        .default_button()
}

/// A phone's rename rests on the bottom edge as every phone sheet does,
/// right above the keyboard its field raises: the field, then Save the
/// width of the sheet under the thumb. Its ✕, a swipe down or the scrim
/// cancels.
fn phone_rename(desk: &Desk, title: &'static str, t: &Tokens) -> El {
    let s = &t.space;
    let save = save_button("Save", t).w_full().min_h(s.touch_min);
    let body = rune::col()
        .w_full()
        .px(s.s4)
        .pt(s.s1)
        .gap(s.s3)
        .child(rename_field(desk, t).min_h(s.touch_min))
        .child(save);
    let sheet = super::phone::sheet::BottomSheet {
        which: crate::desk::phone::dismiss::Bottom::Rename,
        title,
        close: Desk::cancel_rename,
        body,
        foot: None,
    };
    super::phone::sheet::bottom(desk, sheet, t)
}

#[cfg(test)]
mod tests;
