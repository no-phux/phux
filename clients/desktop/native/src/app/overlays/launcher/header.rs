//! The palette's header: the hub's mode tabs and each mode's controls.
//!
//! A mode's controls depend on the mode and the query, never on the
//! highlighted row, so moving the selection never moves a button.

use gpui::prelude::*;
use gpui::{AnyElement, Context, IntoElement, Styled, WeakEntity, Window, div, px};
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};

use crate::app::color::from_hex;
use crate::app::desk::{Desk, Modal};
use crate::settings::theme::Palette;

use super::super::goto_filter::GotoFilter;

pub(super) struct Header {
    pub(super) modal: Modal,
    pub(super) all_commands: bool,
    pub(super) details: bool,
    pub(super) filter: GotoFilter,
    pub(super) empty_query: bool,
    pub(super) compact: bool,
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
            empty_query: false,
            compact: false,
        },
        colors,
        &cx.weak_entity(),
    )
    .into_any_element()
}

pub(super) fn header(state: &Header, colors: &Palette, handle: &WeakEntity<Desk>) -> gpui::Div {
    let modal = state.modal.clone();
    let modes = div()
        .flex()
        .items_center()
        .gap(px(2.))
        .when(
            matches!(
                modal,
                Modal::Commands | Modal::Goto
            ),
            |modes| {
                modes.children(
                    [
                        (Modal::Goto, "Work", "palette-mode-goto"),
                        (Modal::Commands, "Actions", "palette-mode-commands"),
                    ]
                    .into_iter()
                    .map(|(choice, label, id)| {
                        let handle = handle.clone();
                        let current = modal == choice;
                        div().debug_selector(move || id.into()).child(
                            Button::new(id)
                                .label(label)
                                .ghost()
                                .xsmall()
                                .toggled(current)
                                // The mode in use reads as chosen: filled and
                                // in full ink; the others step back.
                                .when(current, |mode| {
                                    mode.bg(from_hex(&colors.active))
                                        .text_color(from_hex(&colors.theme.foreground))
                                })
                                .when(!current, |mode| {
                                    mode.text_color(from_hex(&colors.subtext))
                                })
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
        // A plain title sits where a mode button's label would.
        .when(modal == Modal::WindowSession, |modes| {
            modes.child(div().px(px(4.)).child("Sessions"))
        })
        .when(modal == Modal::MoveResources, |modes| {
            modes.child(div().px(px(4.)).child("Move to"))
        })
        .when(modal == Modal::Windows, |modes| {
            modes.child(div().debug_selector(|| "palette-windows-title".into()).px(px(4.)).child("Windows"))
        });
    let settings_handle = handle.clone();
    let controls = header_controls(state, handle).when(
        matches!(modal, Modal::Goto | Modal::Commands | Modal::Windows),
        |controls| {
            controls.child(
                div()
                    .debug_selector(|| "palette-open-settings".into())
                    .child(
                        Button::new("palette-open-settings")
                            .icon(gpui_kit::component::IconName::Settings)
                            .ghost()
                            .xsmall()
                            .tooltip("Settings · ⌘,")
                            .on_click(move |_, window, cx| {
                                let _ = settings_handle
                                    .update(cx, |desk, cx| desk.execute("settings", window, cx));
                            }),
                    ),
            )
        },
    );
    let wrapped = wraps_controls(&modal, state.compact, state.empty_query);
    div()
        .h(px(if wrapped { 68. } else { 36. }))
        .when(wrapped, |this| this.flex_wrap())
        // With the xsmall buttons' 4pt label padding, labels sit 12pt in,
        // on the search field's and the rows' text column.
        .px(px(8.))
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

pub(super) fn wraps_controls(modal: &Modal, compact: bool, empty_query: bool) -> bool {
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
