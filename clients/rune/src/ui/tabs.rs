//! The tab strip, shown once a session has more than one tab, and the
//! rename sheet.

use rune::{El, TextStyle, col, el, row, text, text_input};

use super::{CAPTION, Ink, Sheet, button, icon_press, icons, pressable, tip};
use crate::activity::Mark;
use crate::desk::{Desk, Renaming, Tab};
use crate::keys::{self, Shortcut};

/// A tab's name: one weight whether or not it is the active one, so
/// switching never moves a neighbour.
const NAME: TextStyle = TextStyle::new(12.0, 500.0);
const HEIGHT: f32 = 26.0;
/// The widest a tab grows, and the least it keeps: the start of its name.
const WIDEST: f32 = 208.0;
const NARROWEST: f32 = 30.0;
/// The active tab stays readable however many crowd it.
const ACTIVE_LEAST: f32 = 124.0;
/// The gap a divider stands in, and the new-tab button after the strip.
const BETWEEN: f32 = 5.0;
const AFTER: f32 = 30.0;
/// Narrower than this, a resting tab is its name alone: no chord, and no
/// close button waiting under the pointer for room the name needs.
const ROOMY: f32 = 132.0;

/// A resting tab's share of `room` points among `count`, never past the
/// widest. The active tab keeps its least first, so a crowd squeezes the
/// others. A bar not laid out yet has no room to report and gives every tab
/// its fill.
fn share(room: f32, count: usize) -> f32 {
    if room <= 0.0 || count == 0 {
        return WIDEST;
    }
    let rest = count.saturating_sub(1) as f32;
    let free = room - AFTER - BETWEEN * rest;
    let even = free / count as f32;
    if even >= ACTIVE_LEAST || rest == 0.0 {
        return even.min(WIDEST);
    }
    ((free - ACTIVE_LEAST) / rest).clamp(NARROWEST, WIDEST)
}

/// Whether `count` tabs at their least still overrun `room`.
fn crowded(room: f32, count: usize) -> bool {
    let rest = count.saturating_sub(1) as f32;
    room > 0.0 && ACTIVE_LEAST + rest * (NARROWEST + BETWEEN) + AFTER > room
}

/// The chord that reaches the tab at `index`, where the platform writes
/// chords short enough to sit in a tab (⌘1; `Ctrl+Shift+1` waits in the
/// tooltip).
fn chord(index: usize) -> Option<String> {
    let digit = u8::try_from(index).ok().filter(|i| *i < 9)?;
    let hint = keys::command(Shortcut::Tab(digit))?.hint();
    (hint.chars().count() <= 3).then_some(hint)
}

/// The tab's trailing slot: its chord at rest and its close button under the
/// pointer, in the same place so neither moves the name. The active tab
/// always shows its close.
fn trailing(index: usize, active: bool, width: f32, ink: &Ink) -> Option<El> {
    if !active && width < ROOMY {
        return None;
    }
    let close = pressable(format!("tab-close-{index}"), ink)
        .size(16.0, 16.0)
        .radius(5.0)
        .center()
        .label("Close tab")
        .on_press(move |d: &mut Desk| {
            // The question a busy tab asks is about the tab on screen.
            d.select_tab(index);
            d.close_tab(index);
        })
        .child(rune::icon(&icons::CLOSE, 10.0).color(ink.dim).a11y_hidden());
    let close = tip(close, "Close tab", Shortcut::CloseTab);
    if active {
        return Some(close);
    }
    let hint = chord(index).map(|hint| {
        text(hint)
            .font(CAPTION)
            .color(ink.dim.alpha(0.7))
            .group_opacity(1.0, 0.0)
            .a11y_hidden()
    });
    Some(
        row()
            .h(16.0)
            .min_w(16.0)
            .items_center()
            .justify_end()
            .children(hint)
            .child(
                close
                    .pin(None, Some(0.0), Some(0.0), None)
                    .group_opacity(0.0, 1.0),
            ),
    )
}

fn tab(desk: &Desk, tab: &Tab, index: usize, width: f32, ink: &Ink) -> El {
    let waiting = tab
        .terminals
        .iter()
        .any(|id| desk.activities.mark(id).needs_you());
    let dot = waiting.then(|| {
        let alert = super::sidebar::mark_color(Mark::Blocked, ink);
        el().size(6.0, 6.0)
            .radius(3.0)
            .fill(alert)
            .shadow(0.0, 6.0, alert.alpha(0.55))
            .a11y_hidden()
    });
    let label = desk.tab_label(tab);
    let name = text(label.clone())
        .font(NAME)
        .color(if tab.active { ink.text } else { ink.dim })
        // A long name fades out, as a browser's does: no ellipsis taking
        // letters from a narrow tab.
        .truncate_fade_width(1.2);
    let name = if tab.active {
        name
    } else {
        name.group_color(ink.text)
    };
    let roomy = width >= ROOMY;
    let base = pressable(format!("tab-{index}"), ink)
        .h(HEIGHT)
        .pl(if roomy { 10.0 } else { 7.0 })
        .pr(if roomy || tab.active { 7.0 } else { 5.0 })
        .gap(7.0)
        .radius(8.0)
        .continuous_corners()
        .items_center()
        .hover_group()
        .on_press(move |d: &mut Desk| d.select_tab(index))
        .on_double_press(move |d: &mut Desk| d.rename_tab(index));
    let base = if tab.active {
        // The one you are on never gives up its name to a crowd. It is lit
        // as the keyboard's card is: the rim runs into the machine's colour.
        base.fill(ink.current)
            .border_gradient(1.0, ink.text.alpha(0.18), ink.host.alpha(0.5))
            .shadow(1.0, 3.0, ink.shade)
            .shadow(0.0, 12.0, ink.host.alpha(0.12))
            .max_w(width.max(ACTIVE_LEAST))
    } else {
        // `shrink` clears the floor, so the floor is set after it.
        base.max_w(width).shrink().min_w(NARROWEST.min(width))
    };
    let digit = u8::try_from(index).ok().filter(|i| *i < 9);
    let base = match digit {
        Some(digit) => tip(base, label, Shortcut::Tab(digit)),
        None => base.help(label),
    };
    base.children(dot)
        .child(name)
        .child(el().grow(1.0))
        .children(trailing(index, tab.active, width, ink))
}

/// The hairline between two resting tabs. Beside the active tab it keeps its
/// room and draws nothing, so the row never shifts as the tab moves.
fn divider(shown: bool, ink: &Ink) -> El {
    let line = el().w(1.0).h(12.0).radius(0.5);
    el().w(BETWEEN)
        .h(HEIGHT)
        .center()
        .a11y_hidden()
        .child(if shown {
            line.fill(ink.text.alpha(0.1))
        } else {
            line
        })
}

pub fn strip(desk: &Desk, ink: &Ink) -> Option<El> {
    let tabs = &desk.tabs.list;
    if tabs.len() < 2 {
        return None;
    }
    let width = share(desk.tabs.room, tabs.len());
    let mut line = row().key("tabs").shrink().min_w(0.0).items_center();
    if crowded(desk.tabs.room, tabs.len()) {
        // More tabs than the bar holds fade out at its end, never over it.
        line = line.clip_fade(0.0, 0.0, 14.0, 0.0);
    }
    for (index, item) in tabs.iter().enumerate() {
        if index > 0 {
            let beside_active = item.active || tabs[index - 1].active;
            line = line.child(divider(!beside_active, ink));
        }
        line = line.child(tab(desk, item, index, width, ink));
    }
    let add = icon_press(&icons::PLUS, "New tab", "tab-new", ink, |d: &mut Desk| {
        d.new_tab();
    });
    Some(
        row()
            .shrink()
            .min_w(0.0)
            .gap(4.0)
            .items_center()
            .child(line)
            .child(tip(add, "New tab", Shortcut::NewTerminal)),
    )
}

pub fn rename_sheet(desk: &Desk, ink: &Ink) -> Option<El> {
    let title = match desk.tabs.renaming.as_ref()? {
        Renaming::Session(_) => "Rename session",
        Renaming::Tab(_) => "Rename tab",
    };
    let field = row()
        .w_full()
        .px(10.0)
        .py(7.0)
        .radius(7.0)
        .fill(ink.separator)
        .child(
            text_input(
                &desk.tabs.rename_field,
                TextStyle::new(14.0, 400.0).lh(20.0),
            )
            .key("rename-field")
            .single_line()
            .grow(1.0)
            .color(ink.text)
            .on_edit(|d: &mut Desk| &mut d.tabs.rename_field)
            .on_input_key(|d: &mut Desk, event, tx| d.rename_key(event, tx))
            .on_submit(|d: &mut Desk, tx| d.submit_rename(tx)),
        );
    // A phone has no Return or Esc to name.
    let (save_words, cancel_words) = if desk.phone.is_some() {
        ("Save", "Cancel")
    } else {
        ("Save (Return)", "Cancel (Esc)")
    };
    let save = pressable("rename-save", ink)
        .px(12.0)
        .py(6.0)
        .radius(6.0)
        .fill(ink.accent)
        .focusable(true)
        .role("button")
        .label("Save")
        .default_button()
        .on_press_tx(|d: &mut Desk, tx| d.submit_rename(tx))
        .child(text(save_words).font(CAPTION).color(ink.on_accent));
    let cancel = button(cancel_words, "rename-cancel", ink, Desk::cancel_rename)
        .grow(0.0)
        .px(12.0)
        .focusable(true)
        .role("button")
        .label("Cancel");
    let foot = row()
        .w_full()
        .gap(8.0)
        .child(el().grow(1.0))
        .child(cancel)
        .child(save);
    Some(
        Sheet {
            key: "rename",
            title,
            width: 360.0,
            top: 140.0,
            scrim: 0.3,
            close: Desk::cancel_rename,
        }
        .build(None, col().w_full().padding(16.0).child(field), foot, ink),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(count: u8, active: u8) -> Vec<Tab> {
        (0..count)
            .map(|i| Tab {
                id: [i; 16],
                name: format!("a tab with a long name {i}"),
                terminals: Vec::new(),
                active: i == active,
            })
            .collect()
    }

    fn laid_out(count: u8, active: u8, window: (f32, f32)) -> rune::Runtime<Desk> {
        let mut desk = Desk::settings_test_fixture();
        desk.settings_open = false;
        desk.tabs.list = named(count, active);
        let mut rt = rune::Runtime::new(desk, crate::ui::view, window, rune::Env::new(false, 1.0));
        // The bar reports its room on the first frame; the tabs fit it after.
        for frame in 0..4 {
            rt.frame(f64::from(frame));
        }
        rt
    }

    #[test]
    fn a_crowd_of_tabs_never_runs_under_the_window_controls() {
        for (count, window) in [
            (3, (760.0, 500.0)),
            (12, (760.0, 500.0)),
            (30, (640.0, 400.0)),
        ] {
            let rt = laid_out(count, 1, window);
            assert!(rt.state.tabs.room > 0.0, "the bar reported its room");
            let add = rt.presented("tab-new").expect("the new-tab button").0;
            let bell = rt.presented("news-button").expect("the bell").0;
            assert!(
                add[0] + add[2] <= bell[0],
                "{count} tabs: new-tab ends at {}, the bell starts at {}",
                add[0] + add[2],
                bell[0]
            );
        }
    }

    #[test]
    fn the_active_tab_keeps_its_name_in_a_crowd() {
        let rt = laid_out(12, 5, (900.0, 500.0));
        let active = rt.presented("tab-5").expect("the active tab").0;
        let resting = rt.presented("tab-0").expect("a resting tab").0;
        assert!(active[2] >= ACTIVE_LEAST - 0.5, "active: {active:?}");
        assert!(resting[2] < active[2], "resting: {resting:?}");
        assert!(resting[2] >= NARROWEST - 0.5, "resting: {resting:?}");
    }

    #[test]
    fn tabs_share_the_room_and_stop_at_their_widest() {
        assert_eq!(share(0.0, 3), WIDEST, "before the bar is laid out");
        assert_eq!(share(2000.0, 2), WIDEST);
        let even = share(600.0, 3);
        assert!((ACTIVE_LEAST..WIDEST).contains(&even), "{even}");
        // Past an even split the active tab keeps its least first.
        let squeezed = share(500.0, 8);
        assert!((NARROWEST..ACTIVE_LEAST).contains(&squeezed), "{squeezed}");
        assert_eq!(share(100.0, 20), NARROWEST);
        assert!(crowded(300.0, 20) && !crowded(900.0, 4));
    }
}
