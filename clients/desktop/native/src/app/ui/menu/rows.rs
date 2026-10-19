//! The header and the rows a page renders.

use super::*;

impl GlassMenu {
    pub(super) fn header(
        &self,
        ink: Hsla,
        washes: Washes,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let first = self.first();
        let heading = self.page().heading.clone().unwrap_or_default();
        if first && self.page().heading.is_none() {
            return None;
        }
        let acts = self.header_acts();
        let leads = first && acts;
        let icon = if first {
            heading.icon
        } else {
            Some(IconName::ChevronLeft)
        };
        let selected = acts && self.selected.is_none();
        Some(
            div()
                .id("glass-menu-header")
                .debug_selector(|| "glass-menu-header".into())
                .role(gpui::Role::MenuItem)
                .aria_label(if first {
                    heading.title.clone()
                } else {
                    format!("Back from {}", heading.title).into()
                })
                .aria_selected(selected)
                .when(selected, |el| el.aria_active_descendant())
                .on_mouse_move(cx.listener(|menu, event: &gpui::MouseMoveEvent, _, cx| {
                    menu.hover_select(None, event.position, cx);
                }))
                .h(px(HEADER))
                .w_full()
                .pt(px(INSET))
                .px(px(INSET))
                .when(acts, |el| {
                    el.group("glass-menu-header")
                        .cursor_pointer()
                        .on_click(cx.listener(|menu, _, _, cx| menu.header_click(cx)))
                })
                .child(
                    // Inset and rounded as a row's selection, the disc
                    // centred on the rows' icon column, the chevron on
                    // theirs.
                    div()
                        .size_full()
                        .rounded(px(self.skin.shape.kit_radius_lg))
                        .pl(px(if icon.is_some() {
                            PAD + (ICON_COLUMN - DISC) / 2.
                        } else {
                            PAD
                        }))
                        .pr(px(PAD))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .when(selected, |el| el.bg(washes.quiet))
                        .when(acts, |el| {
                            el.group_hover("glass-menu-header", move |style| style.bg(washes.quiet))
                        })
                        .children(icon.map(|icon| {
                            div()
                                .flex_shrink_0()
                                .size(px(DISC))
                                .rounded_full()
                                .bg(washes.disc)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(icon).size(px(15.)))
                        }))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(px(1.))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(heading.title),
                                )
                                .children(heading.detail.map(|detail| {
                                    div()
                                        .text_size(px(11.))
                                        .text_color(ink.opacity(SECONDARY))
                                        .truncate()
                                        .child(detail)
                                })),
                        )
                        .when(leads, |el| {
                            el.child(
                                Icon::new(IconName::ChevronRight)
                                    .size(px(13.))
                                    .text_color(ink.opacity(MUTED)),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    pub(super) fn row(
        &self,
        index: usize,
        item: &MenuItem,
        ink: Hsla,
        washes: Washes,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let choice = item.choice.clone();
        let row_radius = px(self.skin.shape.kit_radius_lg);
        let selector = item.selector.clone();
        let tone = if item.danger { cx.theme().danger } else { ink };
        div()
            .id(("glass-menu-row", index))
            .role(gpui::Role::MenuItem)
            .aria_label(item.label.clone())
            .aria_selected(self.selected == Some(index))
            .when(self.selected == Some(index), |el| {
                el.aria_active_descendant()
            })
            .debug_selector(move || match &selector {
                Some(id) => format!("glass-menu-{id}"),
                None => format!("glass-menu-row-{index}"),
            })
            .relative()
            .flex_shrink_0()
            .h(px(item.height()))
            .mt(px(item.space()))
            .px(px(PAD))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .text_color(tone)
            .on_mouse_move(
                cx.listener(move |menu, event: &gpui::MouseMoveEvent, _, cx| {
                    menu.hover_select(Some(index), event.position, cx);
                }),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(
                cx.listener(move |menu, _, window, cx| menu.choose(choice.clone(), window, cx)),
            )
            .active(move |style| style.bg(washes.press).rounded(row_radius))
            .child(
                div()
                    .w(px(ICON_COLUMN))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .children(
                        item.icon.map(|icon| {
                            Icon::new(icon).size(px(15.)).text_color(tone.opacity(ICON))
                        }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().truncate().child(item.label.clone()))
                    .children(item.detail.clone().map(|detail| {
                        div()
                            .text_size(px(11.))
                            .text_color(ink.opacity(MUTED))
                            .truncate()
                            .child(detail)
                    })),
            )
            .children(item.chord.clone().map(|chord| {
                div()
                    .text_color(ink.opacity(HINT))
                    .child(Kbd::new(chord).appearance(false))
            }))
            .when(matches!(item.choice, Choice::Page(_)), |el| {
                el.child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(13.))
                        .text_color(ink.opacity(HINT)),
                )
            })
            .when(item.checked, |el| {
                el.child(div().text_color(cx.theme().primary).child("✓"))
            })
            .into_any_element()
    }
}
