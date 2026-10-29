//! Painting the menu each frame.

use super::*;

impl Render for GlassMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.animate_surface(window, cx);
        let skin = self.skin;
        let ink = cx.theme().foreground;
        let washes = Washes::of(ink, &skin);
        let springs = spring(&skin);
        let radius = px(skin.shape.popover_radius);
        let row_radius = px(super::super::list::row_radius(&skin));
        let native = self.native_glass;
        let surface = if skin.material.backdrop == Backdrop::Opaque {
            cx.theme().popover.alpha(1.)
        } else {
            cx.theme().popover
        };
        let page = self.stack.last().expect("first page").clone();
        let highlight = self.selected.map(|index| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .h(px(page.items[index].height()))
                .rounded(row_radius)
                .bg(washes.selection)
                .border_1()
                .border_color(washes.hairline)
                .with_spring(
                    "selection",
                    SpringAnimation::new(springs)
                        .to(px(page.row_y(index)))
                        .with_epsilon(0.1),
                    |el, y| el.top(y),
                )
        });
        let rows = page
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| self.row(index, item, ink, washes, cx))
            .collect::<Vec<_>>();
        let top = self.header_height() + INSET;
        let header = self.header(washes, cx);
        div()
            .id("glass-menu")
            .role(gpui::Role::Menu)
            .aria_label(self.name.clone())
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .rounded(radius)
            .overflow_hidden()
            .text_size(px(13.))
            .text_color(ink)
            .line_height(gpui::relative(super::super::typography::LEADING))
            .when(!native, |el| {
                el.bg(surface).child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(radius)
                        .border_1()
                        .border_color(cx.theme().border),
                )
            })
            .on_key_down(cx.listener(Self::key))
            .on_action(cx.listener(Self::forward))
            .on_mouse_down_out(cx.listener(|menu, _, window, cx| menu.dismiss(true, window, cx)))
            .children(header)
            .child(
                div()
                    .absolute()
                    .top(px(top))
                    .bottom(px(INSET))
                    .left(px(INSET))
                    .right(px(INSET))
                    .id("glass-menu-scroll")
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .children(highlight)
                            .children(rows)
                            .with_spring(
                                ("page", self.page_generation),
                                SpringAnimation::new(springs)
                                    .to(px(0.))
                                    .from(px(8.))
                                    .with_epsilon(0.1),
                                |el, y| el.relative().top(y),
                            ),
                    ),
            )
    }
}
