//! Native command and navigation presentation. Kit owns input, selection,
//! accessibility and action dispatch; the host supplies ranked rows and context.

use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, FontWeight, HighlightStyle, IntoElement, MouseButton, SharedString,
    Styled, StyledText, WeakEntity, Window, div, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem};
use gpui_kit::component::{Icon, IconName, IndexPath, Sizable};

use crate::app::actions::Run;
use crate::app::color::from_hex;
use crate::app::desk::{Desk, Modal};
use crate::settings::theme::Palette;

use super::goto_facts::FactsCard;
use super::goto_filter::GotoFilter;
use super::{
    Row, Section, Target, command_sections, move_sections, prepare_goto, query_terms,
    rank_sections, session_sections, settings_sections,
};

/// Narrowest window, less its margins, that fits the list and the facts card.
/// A narrower window shows the list alone.
const FACTS_MIN_ROOM: f32 = 840.;
/// Width of the metadata card beside the Go To list.
const FACTS_COLUMN: f32 = 280.;

pub(super) fn palette(
    desk: &mut Desk,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some(state) = desk.palette_state.clone() else {
        return div().into_any_element();
    };
    let query = state.read(cx).query(cx).to_string();
    let (placeholder, verb, sections) = sections_for(desk, &query, cx);
    desk.align_goto_highlight(&sections, &state, window, cx);
    let count: usize = sections.iter().map(|section| section.rows.len()).sum();
    let targets: Vec<Vec<Target>> = sections
        .iter()
        .map(|section| section.rows.iter().map(|row| row.target.clone()).collect())
        .collect();
    let query_handle = cx.weak_entity();
    let confirm_handle = query_handle.clone();
    let header_handle = query_handle.clone();
    let selection_handle = query_handle.clone();
    let colors = Rc::new(colors.clone());
    let header_colors = colors.clone();
    let footer_colors = colors.clone();
    let empty_colors = colors.clone();
    let modal = desk.modal.clone();

    let empty_query = query.trim().is_empty();
    let filter = desk.goto.filter;
    let narrowed = desk.modal == Modal::Goto && filter != GotoFilter::All;
    let chrome = if desk.modal == Modal::Goto {
        160.
    } else {
        124.
    };
    let viewport = window.viewport_size();
    let top = (viewport.height.as_f32() * 0.13).clamp(24., 84.);
    let headings = sections
        .iter()
        .filter(|section| section.label.is_some())
        .count()
        .min(4) as f32
        * 32.;
    let rows_height = (count.min(8) as f32 * 54. + headings + 16.).max(152.);
    let wanted = chrome + rows_height;
    let height = (viewport.height.as_f32() - top - 24.)
        .min(wanted)
        .clamp(180., 536.);
    let room = (viewport.width.as_f32() - 32.).max(0.);
    let list_width = room.min(720.);
    let selected = state
        .read(cx)
        .selected_index()
        .or_else(|| (count > 0).then_some(IndexPath::new(0).section(0)));
    // The card is the highlighted row's metadata. Selection only refreshes
    // this presentation; confirm is what opens the session.
    let preview = selected_preview(&desk.modal, selected, &sections);
    let show_facts = preview.is_some() && room >= FACTS_MIN_ROOM;
    let width = if show_facts {
        room.min(list_width + FACTS_COLUMN)
    } else {
        list_width
    };
    let compact = (width - if show_facts { FACTS_COLUMN } else { 0. }) < 560.;
    let mut command = Command::new(&state)
        .filterable(false)
        .bordered(false)
        .rounded(px(if show_facts { 0. } else { 14. }))
        .h(px(height))
        .bg(from_hex(&colors.raised))
        .placeholder(placeholder)
        .max_h(px((height - chrome).max(56.)))
        .header(move |_, _, _| mode_header(&modal, filter, &header_colors, &header_handle))
        .empty(move |_, _, _| empty_results(empty_query, narrowed, &empty_colors))
        .footer(move |_, _, _| palette_footer(count, empty_query, compact, verb, &footer_colors))
        .on_query(move |query, window, cx| {
            let _ = query_handle.update(cx, |desk, cx| desk.palette_query(query, window, cx));
        })
        .on_select(move |_, _, cx| {
            // Kit remains the selection owner. Refresh the facts card only;
            // highlighting does not acquire, resize, focus, or acknowledge.
            let _ = selection_handle.update(cx, |_, cx| cx.notify());
        })
        .on_confirm(move |index: IndexPath, window, cx| {
            let target = targets
                .get(index.section)
                .and_then(|rows| rows.get(index.row))
                .cloned();
            if let Some(target) = target {
                let _ = confirm_handle.update(cx, |desk, cx| desk.pick(target, window, cx));
            }
        });
    command = with_sections(command, sections, &query, selected, &colors);
    let panel = super::card(&colors, width)
        .id("palette-panel")
        .mt(px(top))
        .text_left()
        .overflow_hidden()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    let panel = if let Some(card) = preview.filter(|_| show_facts) {
        panel.child(
            div()
                .flex()
                .h(px(height))
                .w_full()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_hidden()
                        .child(command),
                )
                .child(facts_column(card, &colors)),
        )
    } else {
        panel.child(command)
    };
    panel.into_any_element()
}

fn sections_for(
    desk: &mut Desk,
    query: &str,
    cx: &Context<Desk>,
) -> (&'static str, &'static str, Vec<Section>) {
    match desk.modal {
        Modal::Goto => {
            prepare_goto(desk, query);
            let mut sections = desk.goto_search.cloned_sections();
            if !query.trim().is_empty() && desk.goto.filter == GotoFilter::All {
                sections.extend(rank_sections(command_sections(desk, cx), query));
                sections.extend(rank_sections(settings_sections(), query));
            }
            ("Search work, actions and settings…", "Open", sections)
        }
        Modal::Settings => (
            "Find a setting…",
            "Open",
            rank_sections(settings_sections(), query),
        ),
        Modal::WindowSession => (
            "Find work for a new window…",
            "Open window",
            rank_sections(session_sections(desk), query),
        ),
        Modal::MoveResources => (
            "Move selected work to…",
            "Move work",
            rank_sections(move_sections(desk), query),
        ),
        _ => {
            let mut sections = command_sections(desk, cx);
            if query.trim().is_empty() {
                sections.retain(|section| section.label == Some("Essentials"));
            } else {
                sections.extend(settings_sections());
            }
            ("Search all actions…", "Run", rank_sections(sections, query))
        }
    }
}

fn with_sections(
    mut command: Command,
    sections: Vec<Section>,
    query: &str,
    selected: Option<IndexPath>,
    colors: &Rc<Palette>,
) -> Command {
    let terms = query_terms(query);
    let tokens: Vec<&str> = terms.iter().map(String::as_str).collect();
    for (section_index, section) in sections.into_iter().enumerate() {
        let mut group = CommandGroup::new();
        if let Some(label) = section.label {
            group = group.label(label);
        }
        command = command.group(group.items(section.rows.into_iter().enumerate().map(
            |(row_index, row)| {
                let show_category = section.label != Some(row.category);
                let active = selected == Some(IndexPath::new(row_index).section(section_index));
                item(row, &tokens, colors.clone(), active, show_category)
            },
        )));
    }
    command
}

fn selected_preview(
    modal: &Modal,
    selected: Option<IndexPath>,
    sections: &[Section],
) -> Option<FactsCard> {
    if *modal != Modal::Goto {
        return None;
    }
    let index = selected?;
    sections
        .get(index.section)?
        .rows
        .get(index.row)?
        .preview
        .clone()
}

fn empty_results(empty: bool, narrowed: bool, colors: &Palette) -> AnyElement {
    let title = match (empty, narrowed) {
        (true, true) => "No agents in this state",
        (true, false) => "Nothing here yet",
        _ => "No matching results",
    };
    div()
        .w_full()
        .min_h(px(112.))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .text_color(from_hex(&colors.muted))
        .child(Icon::new(IconName::Search).size(px(22.)))
        .child(div().text_sm().child(title))
        .child(div().text_xs().child("Try a name, action or setting."))
        .into_any_element()
}

fn palette_footer(
    count: usize,
    empty: bool,
    compact: bool,
    verb: &str,
    colors: &Palette,
) -> AnyElement {
    div()
        .id("hub-footer")
        .debug_selector(|| "hub-footer".into())
        .h(px(40.))
        .flex_shrink_0()
        .px(px(14.))
        .border_t_1()
        .border_color(from_hex(&colors.border))
        .flex()
        .items_center()
        .justify_between()
        .text_size(px(11.))
        .text_color(from_hex(&colors.muted))
        .child(format!("{count} results"))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(key("↑ ↓", colors))
                .when(!compact, |row| row.child("Navigate"))
                .child(key("↩", colors))
                .child(verb.to_owned())
                .child(key("esc", colors))
                .when(!compact, |row| {
                    row.child(if empty { "Close" } else { "Clear" })
                }),
        )
        .into_any_element()
}

fn facts_column(card: FactsCard, colors: &Palette) -> impl IntoElement {
    div()
        .debug_selector(|| "palette-facts".into())
        .id("palette-facts")
        .w(px(FACTS_COLUMN))
        .h_full()
        .flex_shrink_0()
        .p(px(16.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .border_l_1()
        .border_color(from_hex(&colors.border))
        .child(
            div()
                .truncate()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(from_hex(&colors.theme.foreground))
                .child(card.title),
        )
        .children(card.facts.into_iter().map(|fact| {
            div()
                .flex()
                .gap(px(12.))
                .min_w_0()
                .child(
                    div()
                        .flex_none()
                        .w(px(84.))
                        .text_size(px(12.))
                        .text_color(from_hex(&colors.muted))
                        .child(fact.name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.))
                        .text_color(from_hex(&colors.theme.foreground))
                        .child(fact.value),
                )
        }))
        .child(
            div()
                .pt(px(10.))
                .text_size(px(11.))
                .text_color(from_hex(&colors.muted))
                .child("Press Enter to open."),
        )
}

pub(crate) fn hub_header(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> AnyElement {
    mode_header(&desk.modal, desk.goto.filter, colors, &cx.weak_entity())
}

fn mode_header(
    modal: &Modal,
    filter: GotoFilter,
    colors: &Palette,
    desk: &WeakEntity<Desk>,
) -> AnyElement {
    let modes = [
        (Modal::Goto, "Work", "palette-mode-goto"),
        (Modal::Commands, "Actions", "palette-mode-commands"),
        (Modal::Settings, "Settings", "palette-mode-settings"),
    ];
    let navigation = matches!(modal, Modal::Goto | Modal::Commands | Modal::Settings);
    let tabs = div()
        .flex()
        .items_center()
        .gap(px(4.))
        .children(modes.into_iter().map(|(mode, label, id)| {
            let handle = desk.clone();
            let active = *modal == mode;
            div().debug_selector(move || id.into()).child(
                Button::new(id)
                    .label(label)
                    .small()
                    .ghost()
                    .toggled(active)
                    .when(active, |button| button.bg(from_hex(&colors.active)))
                    .on_click(move |_, window, cx| {
                        let _ = handle.update(cx, |desk, cx| {
                            if desk.modal != mode {
                                desk.open_modal(mode.clone(), window, cx);
                            }
                        });
                    }),
            )
        }));
    let close = desk.clone();
    div()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(48.))
                .px(px(12.))
                .flex()
                .items_center()
                .justify_between()
                .child(if navigation {
                    tabs.into_any_element()
                } else {
                    div()
                        .text_sm()
                        .child("Choose a destination")
                        .into_any_element()
                })
                .child(
                    Button::new("hub-close")
                        .icon(IconName::Close)
                        .ghost()
                        .small()
                        .tooltip("Close (esc)")
                        .on_click(move |_, window, cx| {
                            let _ = close.update(cx, |desk, cx| desk.close_modal(window, cx));
                        }),
                ),
        )
        .when(*modal == Modal::Goto, |this| {
            this.child(state_filters(filter, colors, desk))
        })
        .into_any_element()
}

fn state_filters(filter: GotoFilter, colors: &Palette, desk: &WeakEntity<Desk>) -> AnyElement {
    div()
        .id("hub-state-filters")
        .overflow_x_scroll()
        .h(px(36.))
        .flex_shrink_0()
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(4.))
        .children(GotoFilter::ALL.map(|choice| {
            let desk = desk.clone();
            let active = choice == filter;
            let wash = colors.active.clone();
            div().debug_selector(|| choice.id().into()).child(
                Button::new(choice.id())
                    .label(choice.label())
                    .small()
                    .ghost()
                    .toggled(active)
                    .when(active, move |this| this.bg(from_hex(&wash)))
                    .on_click(move |_, window, cx| {
                        let _ = desk.update(cx, |desk, cx| {
                            desk.set_goto_filter(choice, window, cx);
                        });
                    }),
            )
        }))
        .into_any_element()
}

fn key(label: impl Into<SharedString>, colors: &Palette) -> gpui::Div {
    div()
        .min_w(px(20.))
        .h(px(20.))
        .px(px(4.))
        .rounded(px(4.))
        .border_1()
        .border_color(from_hex(&colors.border))
        .bg(from_hex(&colors.theme.foreground).opacity(0.035))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.))
        .text_color(from_hex(&colors.subtext))
        .child(label.into())
}

fn item(
    row: Row,
    tokens: &[&str],
    colors: Rc<Palette>,
    selected: bool,
    show_category: bool,
) -> CommandItem {
    let icon = row_icon(&row);
    let destructive = matches!(row.command, Some("terminate" | "kill-session"));
    let title = row.title;
    let highlights = title_matches(&title, tokens);
    let detail = row.detail;
    let chord = row.chord.map(SharedString::from);
    let mut item = CommandItem::new()
        .label(title.clone())
        .keywords([row.keywords]);
    if let Some(id) = row.command {
        item = item.action(Box::new(Run::new(id)));
    }
    item.child(move |_, _| {
        div()
            .debug_selector(move || {
                format!("palette-result-{}", row.command.unwrap_or("destination"))
            })
            .flex()
            .flex_1()
            .min_w_0()
            .min_h(px(38.))
            .border_l_2()
            .border_color(if selected {
                from_hex(&colors.theme.accent)
            } else {
                gpui::transparent_black()
            })
            .pl(px(6.))
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .size(px(30.))
                    .flex_shrink_0()
                    .rounded(px(7.))
                    .bg(from_hex(if selected {
                        &colors.accent_wash
                    } else {
                        &colors.hover
                    }))
                    .border_1()
                    .border_color(from_hex(&colors.theme.foreground).opacity(if selected {
                        0.14
                    } else {
                        0.055
                    }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(icon.clone()).size(px(15.)).text_color(from_hex(
                        if destructive {
                            &colors.theme.danger
                        } else if selected {
                            &colors.theme.accent
                        } else {
                            &colors.subtext
                        },
                    ))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(if selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::MEDIUM
                            })
                            .child(StyledText::new(title.clone()).with_highlights(
                                highlights.iter().cloned().map(|range| {
                                    (
                                        range,
                                        HighlightStyle {
                                            color: Some(from_hex(&colors.theme.accent)),
                                            font_weight: Some(FontWeight::BOLD),
                                            ..Default::default()
                                        },
                                    )
                                }),
                            )),
                    )
                    .children(detail.clone().map(|detail| {
                        div()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(from_hex(&colors.theme.foreground).opacity(0.9))
                            .child(detail)
                    })),
            )
            .when(show_category, |this| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(10.))
                        .text_color(from_hex(&colors.theme.foreground).opacity(0.9))
                        .child(row.category),
                )
            })
            .children(chord.clone().map(|chord| key(chord, &colors)))
    })
}

fn row_icon(row: &Row) -> IconName {
    if let Target::Setting(page) = row.target {
        return page.icon();
    }
    match row.command {
        Some("new" | "new-home" | "new-session" | "new-window") => IconName::Plus,
        Some("settings" | "reload-config" | "import-ghostty") => IconName::Settings2,
        Some("folder" | "insert-path") => IconName::FolderOpen,
        Some("find") => IconName::Search,
        Some("copy") => IconName::Copy,
        Some("terminate" | "kill-session") => IconName::CircleAlert,
        Some("performance-monitor" | "performance-monitor-stop") => IconName::Cpu,
        _ => match row.category {
            "Agents" => IconName::Bot,
            "Panes" => IconName::LayoutDashboard,
            "Tabs" => IconName::GalleryVerticalEnd,
            "View" => IconName::Frame,
            "Work" | "Sessions" | "Organization" => IconName::Folder,
            "Connection" => IconName::Network,
            _ => IconName::SquareTerminal,
        },
    }
}

/// Map folded matches back to whole UTF-8 characters, including expanding lowercase forms.
fn title_matches(title: &str, tokens: &[&str]) -> Vec<Range<usize>> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut folded = String::with_capacity(title.len());
    let mut origins = Vec::with_capacity(title.len());
    for (start, ch) in title.char_indices() {
        let original = start..start + ch.len_utf8();
        for ch in ch.to_lowercase() {
            folded.push(ch);
            origins.extend(std::iter::repeat_n(original.clone(), ch.len_utf8()));
        }
    }
    let mut matches = Vec::new();
    for token in tokens {
        if let Some(start) = folded.find(token) {
            matches.push(origins[start].start..origins[start + token.len() - 1].end);
        } else {
            let mut cursor = 0;
            let mut found = Vec::new();
            for ch in token.chars() {
                let Some(offset) = folded[cursor..].find(ch) else {
                    found.clear();
                    break;
                };
                let start = cursor + offset;
                found.push(origins[start].clone());
                cursor = start + ch.len_utf8();
            }
            matches.extend(found);
        }
    }
    matches.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(matches.len());
    for range in matches {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::title_matches;

    #[test]
    fn search_highlights_preserve_unicode_boundaries_and_merge_overlapping_hits() {
        assert_eq!(
            title_matches("İstanbul 東京", &["i", "i\u{307}", "東京"]),
            vec![0..2, 10..16]
        );
        assert_eq!(title_matches("Split Right", &["spr"]), vec![0..2, 6..7]);
        assert_eq!(title_matches("Split Right", &["right", "igh"]), vec![6..11]);
        assert!(title_matches("Split Right", &["zzz"]).is_empty());
    }
}
