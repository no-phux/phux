//! Native command and navigation presentation. Kit owns input, selection,
//! accessibility and action dispatch; the host supplies ranked rows and context.

use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, BoxShadow, Context, FontWeight, HighlightStyle, IntoElement, MouseButton,
    SharedString, Styled, StyledText, WeakEntity, Window, div, point, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem};
use gpui_kit::component::{Icon, IconName, IndexPath, Sizable};

use crate::app::actions::Run;
use crate::app::color::from_hex;
use crate::app::desk::{Desk, Modal};
use crate::settings::theme::Palette;

use super::goto_filter::GotoFilter;
use super::{
    Row, Target, command_sections, move_sections, prepare_goto, query_terms, rank_sections,
    session_sections,
};

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
    let (placeholder, verb, sections) = match desk.modal {
        Modal::Goto => {
            prepare_goto(desk, &query);
            (
                "Search terminals, agents and tabs…",
                "Go to",
                desk.goto_search.cloned_sections(),
            )
        }
        Modal::WindowSession => (
            "Find a session to open in a window…",
            "Open window",
            rank_sections(session_sections(desk), &query),
        ),
        Modal::MoveResources => (
            "Find a destination for selected work…",
            "Move work",
            rank_sections(move_sections(desk), &query),
        ),
        _ => (
            "What would you like to do?",
            "Run command",
            rank_sections(command_sections(desk, cx), &query),
        ),
    };
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
    let remote = desk.remote.is_some();
    let host = desk.remote.as_deref().unwrap_or("Local");
    let context = SharedString::from(if desk.session.is_empty() {
        host.to_owned()
    } else {
        format!("{host} / {}", desk.session)
    });
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
    let height = (viewport.height.as_f32() - top - 24.).clamp(180., 536.);
    let width = (viewport.width.as_f32() - 32.).clamp(0., 720.);
    let compact = width < 560.;
    let selected = state
        .read(cx)
        .selected_index()
        .or_else(|| (count > 0).then_some(IndexPath::new(0).section(0)));
    let mut command = Command::new(&state)
        .filterable(false)
        .bordered(false)
        .rounded(px(14.))
        .h(px(height))
        .bg(from_hex(&colors.raised))
        .placeholder(placeholder)
        .max_h(px((height - chrome).max(56.)))
        .header(move |_, _, _| {
            header(
                &modal,
                &context,
                remote,
                filter,
                &header_colors,
                &header_handle,
            )
        })
        .empty(move |_, _, _| {
            div()
                .w_full()
                .h_full()
                .min_h(px(112.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_color(from_hex(&empty_colors.muted))
                .child(Icon::new(IconName::Search).size(px(22.)))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(from_hex(&empty_colors.theme.foreground))
                        .child(if empty_query && narrowed {
                            "No agents in this state"
                        } else if empty_query {
                            "Nothing to navigate to yet"
                        } else {
                            "No matching results"
                        }),
                )
                .child(div().text_xs().child(if empty_query {
                    "Open a terminal to make it available here."
                } else {
                    "Try a command name, an action, or a shorter search."
                }))
        })
        .footer(move |_, _, _| {
            div()
                .h(px(44.))
                .flex_shrink_0()
                .px(px(14.))
                .border_t_1()
                .border_color(from_hex(&footer_colors.border))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .text_size(px(11.))
                .text_color(from_hex(&footer_colors.theme.foreground).opacity(0.9))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(format!("{count:02}"))
                        .when(!compact, |this| {
                            this.child(if count == 1 { "result" } else { "results" })
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .child(key("↑", &footer_colors))
                        .child(key("↓", &footer_colors))
                        .when(!compact, |this| this.child("Navigate")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .opacity(if count == 0 { 0.4 } else { 1. })
                                .child(key("↩", &footer_colors))
                                .child(verb),
                        )
                        .child(div().w(px(8.)))
                        .child(key("esc", &footer_colors))
                        .when(!compact, |this| {
                            this.child(if empty_query { "Close" } else { "Clear" })
                        }),
                )
        })
        .on_query(move |query, window, cx| {
            let _ = query_handle.update(cx, |desk, cx| desk.palette_query(query, window, cx));
        })
        .on_select(move |_, _, cx| {
            // Kit remains the selection owner; refresh only its presentation.
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
    let terms = query_terms(&query);
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
    div()
        .id("palette-panel")
        .w(px(width))
        .mt(px(top))
        .rounded(px(14.))
        .text_left()
        .border_1()
        .border_color(from_hex(&colors.theme.foreground).opacity(0.14))
        .bg(from_hex(&colors.raised))
        .shadow(vec![BoxShadow {
            color: gpui::black().opacity(if colors.theme.appearance == "light" {
                0.16
            } else {
                0.45
            }),
            offset: point(px(0.), px(20.)),
            blur_radius: px(64.),
            spread_radius: px(-8.),
            inset: false,
        }])
        .overflow_hidden()
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(command)
        .into_any_element()
}

fn header(
    modal: &Modal,
    context: &SharedString,
    remote: bool,
    filter: GotoFilter,
    colors: &Palette,
    desk: &WeakEntity<Desk>,
) -> AnyElement {
    let navigation = *modal == Modal::Goto;
    let commands = desk.clone();
    let goto = desk.clone();
    let modes = div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(2.))
        .when(matches!(modal, Modal::Commands | Modal::Goto), |this| {
            this.child(
                div()
                    .debug_selector(|| "palette-mode-commands".into())
                    .child(
                        Button::new("palette-mode-commands")
                            .label("Commands")
                            .small()
                            .ghost()
                            .toggled(!navigation)
                            .when(!navigation, |this| this.bg(from_hex(&colors.active)))
                            .on_click(move |_, window, cx| {
                                let _ = commands.update(cx, |desk, cx| {
                                    if desk.modal != Modal::Commands {
                                        desk.execute("palette", window, cx);
                                    }
                                });
                            }),
                    ),
            )
            .child(
                div().debug_selector(|| "palette-mode-goto".into()).child(
                    Button::new("palette-mode-goto")
                        .label("Go To")
                        .small()
                        .ghost()
                        .toggled(navigation)
                        .when(navigation, |this| this.bg(from_hex(&colors.active)))
                        .on_click(move |_, window, cx| {
                            let _ = goto.update(cx, |desk, cx| {
                                if desk.modal != Modal::Goto {
                                    desk.execute("goto", window, cx);
                                }
                            });
                        }),
                ),
            )
        })
        .when(*modal == Modal::WindowSession, |this| {
            this.child("Open Session")
        })
        .when(*modal == Modal::MoveResources, |this| {
            this.child("Move Selected Work")
        });
    let context = div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(11.))
        .text_color(from_hex(&colors.theme.foreground).opacity(0.9))
        .child(
            Icon::new(if remote {
                IconName::Globe
            } else {
                IconName::HardDrive
            })
            .xsmall(),
        )
        .child(div().truncate().child(context.clone()));
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
                .gap(px(16.))
                .child(modes)
                .child(context),
        )
        .when(navigation, |this| {
            this.child(state_filters(filter, colors, desk))
        })
        .into_any_element()
}

fn state_filters(filter: GotoFilter, colors: &Palette, desk: &WeakEntity<Desk>) -> AnyElement {
    div()
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
            "Sessions" | "Organization" => IconName::Folder,
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
