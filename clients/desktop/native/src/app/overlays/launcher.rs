//! Search first, with a small launch list and optional navigation details.
//! Kit owns keyboard selection, accessibility and command dispatch.

use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, FontWeight, HighlightStyle, Hsla, IntoElement, Styled, StyledText,
    WeakEntity, Window, div, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{IndexPath, Sizable};

use crate::app::actions::Run;
use crate::app::color::from_hex;
use crate::app::desk::{Desk, Modal};
use crate::settings::theme::Palette;

use super::goto_facts::FactsCard;
use super::goto_filter::GotoFilter;
use super::{
    Row, Section, Target, WINDOWS, command_results, command_sections, corner, move_sections,
    prepare_goto, query_terms, rank_sections, session_sections, settings_sections, window_sections,
};

struct Header {
    modal: Modal,
    all_commands: bool,
    details: bool,
    filter: GotoFilter,
    has_details: bool,
    empty_query: bool,
    compact: bool,
}

fn results(desk: &mut Desk, query: &str, cx: &Context<Desk>) -> (&'static str, Vec<Section>) {
    match desk.modal {
        Modal::Goto => {
            prepare_goto(desk, query);
            let mut sections = desk.goto_search.cloned_sections();
            if !query.trim().is_empty() && desk.goto.filter == GotoFilter::All {
                sections.extend(rank_sections(command_sections(desk, cx), query));
                sections.extend(rank_sections(settings_sections(), query));
                sections.extend(rank_sections(
                    window_sections(desk, cx.entity_id(), cx),
                    query,
                ));
            }
            ("Search work, actions and settings…", sections)
        }
        Modal::Windows => (
            "Find a window or tab…",
            rank_sections(window_sections(desk, cx.entity_id(), cx), query),
        ),
        Modal::Settings => ("Find a setting…", rank_sections(settings_sections(), query)),
        Modal::WindowSession => (
            "Open session…",
            rank_sections(session_sections(desk), query),
        ),
        Modal::MoveResources => ("Move to…", rank_sections(move_sections(desk), query)),
        _ => ("Search commands…", command_results(desk, cx, query)),
    }
}

fn selected_preview(sections: &[Section], selected: Option<IndexPath>) -> Option<FactsCard> {
    let selected = selected?;
    sections
        .get(selected.section)?
        .rows
        .get(selected.row)?
        .preview
        .clone()
}

fn preview_available(sections: &[Section], selected: Option<IndexPath>) -> bool {
    selected
        .and_then(|index| sections.get(index.section)?.rows.get(index.row))
        .is_some_and(|row| row.preview.is_some())
}

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
    let (placeholder, sections) = results(desk, &query, cx);
    desk.align_goto_highlight(&sections, &state, window, cx);
    let count: usize = sections.iter().map(|section| section.rows.len()).sum();
    let targets: Vec<Vec<Target>> = sections
        .iter()
        .map(|section| section.rows.iter().map(|row| row.target.clone()).collect())
        .collect();
    let selected = state
        .read(cx)
        .selected_index()
        .or_else(|| (count > 0).then_some(IndexPath::new(0).section(0)));
    let preview = selected_preview(&sections, selected);
    let details = preview.filter(|_| desk.palette_view.details);
    let viewport = window.viewport_size();
    let top = super::sheet_top(viewport);
    let available = (viewport.height.as_f32() - top - super::SHEET_GUTTER).max(120.);
    let detail_height = if details.is_some() {
        (available * 0.4).min(160.)
    } else {
        0.
    };
    let headings = sections
        .iter()
        .filter(|section| section.label.is_some())
        .count();
    let compact = viewport.width.as_f32() < 452.;
    let extra_header = if wraps_controls(&desk.modal, compact, query.trim().is_empty()) {
        32.
    } else {
        0.
    };
    let list_height = (80. + extra_header + count.clamp(1, 8) as f32 * 36. + headings as f32 * 28.)
        .min(400.)
        .min(available - detail_height);
    let width = (viewport.width.as_f32() - 2. * super::SHEET_GUTTER).clamp(0., 560.);
    let look = desk.look();
    let handle = cx.weak_entity();
    let confirm = handle.clone();
    let selection = handle.clone();
    let colors = Rc::new(colors.clone());
    let empty_colors = colors.clone();
    let header_state = Header {
        modal: desk.modal.clone(),
        all_commands: desk.palette_view.all_commands,
        details: desk.palette_view.details,
        filter: desk.goto.filter,
        has_details: preview_available(&sections, selected),
        empty_query: query.trim().is_empty(),
        compact,
    };
    let header_handle = cx.weak_entity();
    let header_colors = colors.clone();
    let mut command = Command::new(&state)
        .filterable(false)
        .bordered(false)
        .rounded(corner(&look, 12.))
        .h(px(list_height))
        .bg(from_hex(&colors.raised))
        .placeholder(placeholder)
        .max_h(px((list_height - 72. - extra_header).max(36.)))
        .header(move |_, _, _| header(&header_state, &header_colors, &header_handle))
        .empty(move |_, _, _| {
            div()
                .w_full()
                .py(px(24.))
                .px(px(16.))
                .text_sm()
                .text_color(from_hex(&empty_colors.muted))
                .child("No results")
        })
        .on_query(move |query, window, cx| {
            let _ = handle.update(cx, |desk, cx| desk.palette_query(query, window, cx));
        })
        .on_select(move |_, _, cx| {
            let _ = selection.update(cx, |_, cx| cx.notify());
        })
        .on_confirm(move |index: IndexPath, window, cx| {
            let target = targets
                .get(index.section)
                .and_then(|rows| rows.get(index.row))
                .cloned();
            if let Some(target) = target {
                let _ = confirm.update(cx, |desk, cx| desk.pick(target, window, cx));
            }
        });
    let terms = query_terms(&query);
    let tokens: Vec<&str> = terms.iter().map(String::as_str).collect();
    for (section_index, section) in sections.into_iter().enumerate() {
        let mut group = CommandGroup::new();
        if let Some(label) = section.label {
            group = group.label(label);
        }
        // Unranked, a window's tabs sit indented under it.
        let overview = section.label == Some(WINDOWS);
        command = command.group(group.items(section.rows.into_iter().enumerate().map(
            |(row_index, row)| {
                let active = selected == Some(IndexPath::new(row_index).section(section_index));
                let nested = overview && matches!(row.target, Target::Window { tab: Some(_), .. });
                item(row, &tokens, colors.clone(), active, nested)
            },
        )));
    }
    super::sheet(&look, width)
        .id("palette-panel")
        .debug_selector(|| "palette-panel".into())
        .text_left()
        .overflow_hidden()
        .child(command)
        .children(details.map(|card| facts_panel(card, detail_height, &colors, look.border)))
        .into_any_element()
}

fn facts_panel(card: FactsCard, height: f32, colors: &Palette, rule: Hsla) -> impl IntoElement {
    div()
        .id("palette-facts")
        .debug_selector(|| "palette-facts".into())
        .w_full()
        .h(px(height))
        .overflow_y_scroll()
        .p(px(14.))
        .border_t_1()
        .border_color(rule)
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .truncate()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
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
                        .w(px(72.))
                        .text_size(px(11.))
                        .text_color(from_hex(&colors.muted))
                        .child(fact.name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(from_hex(&colors.theme.foreground))
                        .child(fact.value),
                )
        }))
}

fn focus_search(desk: &Desk, window: &mut Window, cx: &mut Context<Desk>) {
    if let Some(state) = desk.palette_state.clone() {
        state.update(cx, |state, cx| state.focus(window, cx));
    }
    cx.notify();
}

pub(crate) fn hub_header(desk: &Desk, colors: &Palette, cx: &mut Context<Desk>) -> AnyElement {
    header(
        &Header {
            modal: desk.modal.clone(),
            all_commands: false,
            details: false,
            filter: desk.goto.filter,
            has_details: false,
            empty_query: false,
            compact: false,
        },
        colors,
        &cx.weak_entity(),
    )
    .into_any_element()
}

fn header(state: &Header, colors: &Palette, handle: &WeakEntity<Desk>) -> gpui::Div {
    let modal = state.modal.clone();
    let modes = div()
        .flex()
        .items_center()
        .gap(px(2.))
        .when(
            matches!(
                modal,
                Modal::Commands | Modal::Goto | Modal::Settings | Modal::Windows
            ),
            |modes| {
                modes.children(
                    [
                        (Modal::Goto, "Work", "palette-mode-goto"),
                        (Modal::Commands, "Actions", "palette-mode-commands"),
                        (Modal::Settings, "Settings", "palette-mode-settings"),
                        (Modal::Windows, "Windows", "palette-mode-windows"),
                        (Modal::Capture, "Capture !", "palette-mode-capture"),
                    ]
                    .into_iter()
                    .map(|(choice, label, id)| {
                        let handle = handle.clone();
                        div().debug_selector(move || id.into()).child(
                            Button::new(id)
                                .label(label)
                                .ghost()
                                .xsmall()
                                .toggled(modal == choice)
                                .on_click(move |_, window, cx| {
                                    let _ = handle.update(cx, |desk, cx| {
                                        if desk.modal != choice {
                                            desk.open_modal(choice.clone(), window, cx);
                                        }
                                    });
                                }),
                        )
                    }),
                )
            },
        )
        .when(modal == Modal::WindowSession, |modes| {
            modes.child("Sessions")
        })
        .when(modal == Modal::MoveResources, |modes| {
            modes.child("Move to")
        });
    let controls = header_controls(state, handle);
    let wrapped = wraps_controls(&modal, state.compact, state.empty_query);
    div()
        .h(px(if wrapped { 68. } else { 36. }))
        .when(wrapped, |this| this.flex_wrap())
        .px(px(10.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .text_size(px(11.))
        .text_color(from_hex(&colors.muted))
        .child(modes)
        .child(controls.when(wrapped, |this| this.w_full().justify_end()))
}

fn wraps_controls(modal: &Modal, compact: bool, empty_query: bool) -> bool {
    compact && (*modal == Modal::Goto || *modal == Modal::Commands && empty_query)
}

fn header_controls(state: &Header, handle: &WeakEntity<Desk>) -> gpui::Div {
    let modal = &state.modal;
    let mut controls = div().flex().items_center().gap(px(4.));
    if *modal == Modal::Commands && state.empty_query {
        let handle = handle.clone();
        controls = controls.child(
            div()
                .debug_selector(|| "palette-all-commands".into())
                .child(
                    Button::new("palette-all-commands")
                        .label("All commands")
                        .ghost()
                        .xsmall()
                        .toggled(state.all_commands)
                        .on_click(move |_, window, cx| {
                            let _ = handle.update(cx, |desk, cx| {
                                desk.palette_view.all_commands = !desk.palette_view.all_commands;
                                focus_search(desk, window, cx);
                            });
                        }),
                ),
        );
    }
    if *modal == Modal::Goto {
        controls = controls.child(state_filter(state.filter, handle));
        if state.has_details {
            let handle = handle.clone();
            controls = controls.child(
                div().debug_selector(|| "palette-details".into()).child(
                    Button::new("palette-details")
                        .label("Details")
                        .ghost()
                        .xsmall()
                        .toggled(state.details)
                        .on_click(move |_, window, cx| {
                            let _ = handle.update(cx, |desk, cx| {
                                desk.palette_view.details = !desk.palette_view.details;
                                focus_search(desk, window, cx);
                            });
                        }),
                ),
            );
        }
    }
    controls
}

fn state_filter(filter: GotoFilter, desk: &WeakEntity<Desk>) -> impl IntoElement {
    let desk = desk.clone();
    div().debug_selector(|| "palette-filter".into()).child(
        Button::new("palette-filter")
            .label(if filter == GotoFilter::All {
                "Filter"
            } else {
                filter.label()
            })
            .ghost()
            .xsmall()
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for choice in GotoFilter::ALL {
                    let desk = desk.clone();
                    menu = menu.item(
                        PopupMenuItem::new(choice.label())
                            .checked(choice == filter)
                            .on_click(move |_, window, cx| {
                                let _ = desk.update(cx, |desk, cx| {
                                    desk.set_goto_filter(choice, window, cx)
                                });
                            }),
                    );
                }
                menu
            }),
    )
}

fn item(
    row: Row,
    tokens: &[&str],
    colors: Rc<Palette>,
    selected: bool,
    nested: bool,
) -> CommandItem {
    let destructive = matches!(row.command, Some("terminate" | "kill-session"));
    let title = row.title;
    let highlights = title_matches(&title, tokens);
    // A destination's context disambiguates it; command explanations remain searchable only.
    let detail = row.detail.filter(|_| row.command.is_none());
    let chord = row.chord.filter(|_| selected);
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
            .id("palette-result-content")
            .role(gpui::Role::Label)
            .aria_label(title.clone())
            .flex()
            .flex_1()
            .min_w_0()
            .h(px(24.))
            .when(nested, |row| row.pl(px(16.)))
            .items_center()
            .gap(px(12.))
            .text_size(px(13.))
            .text_color(from_hex(if destructive {
                &colors.theme.danger
            } else {
                &colors.theme.foreground
            }))
            .child(div().flex_1().min_w_0().truncate().child(
                StyledText::new(title.clone()).with_highlights(highlights.iter().cloned().map(
                    |range| {
                        (
                            range,
                            HighlightStyle {
                                font_weight: Some(FontWeight::MEDIUM),
                                ..Default::default()
                            },
                        )
                    },
                )),
            ))
            .children(detail.clone().map(|detail| {
                div()
                    .max_w(px(220.))
                    .truncate()
                    .text_size(px(11.))
                    .text_color(from_hex(&colors.muted))
                    .child(detail)
            }))
            .children(chord.clone().map(|chord| {
                let selector = format!("palette-chord:{chord}");
                div()
                    .debug_selector(move || selector)
                    .text_size(px(11.))
                    .text_color(from_hex(&colors.muted))
                    .child(chord)
            }))
    })
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
