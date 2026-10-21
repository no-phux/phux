//! A pane's floating controls capsule and its file-drop hint.

use gpui::prelude::*;
use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, Window, div,
    linear_color_stop, linear_gradient, px, relative,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::tooltip::Tooltip;

use crate::connection::AgentInfo;
use crate::workspace::layout::Placement;

use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::pane_identity::{Harness, PaneOrigin};
use crate::app::skin::{Look, Surface, Surfaced};
use crate::app::ui::menu::Anchor;
use crate::app::ui::status::harness_mark;
use crate::app::ui::{agent_swatch, dot, ghost_icon, muted, notice_card};

/// What a pane's capsule offers beyond its fixed actions.
pub(super) struct Offer {
    /// Panes hidden behind this one while it is zoomed.
    pub behind: usize,
    /// The harness whose conversation this pane could show once set up.
    pub setup: Option<Harness>,
}

/// The floating capsule a pane shows at its top edge: who runs there, the
/// terminal/conversation switch (or the step that unlocks it), then split,
/// zoom and the pane's menu, each group set off by a hairline.
pub(super) fn pane_controls(
    desk: &Desk,
    placement: &Placement,
    origin: PaneOrigin,
    zoomed: bool,
    offer: Offer,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let id = placement.id.clone();
    let focus_id = placement.id.clone();
    let middle = if desk.conversation_eligible(&id) {
        Some(view_switch(desk, &id, look, cx).into_any_element())
    } else {
        offer
            .setup
            .map(|harness| setup_button(&id, harness, look, cx).into_any_element())
    };
    div()
        .id(SharedString::from(format!("pane-controls-{id}")))
        .debug_selector({
            let id = id.clone();
            move || format!("pane-controls-{id}")
        })
        .absolute()
        .top(px(7.))
        .right(px(8.))
        .max_w(relative(0.94))
        .h(px(30.))
        .pl(px(10.))
        .pr(px(3.))
        .flex()
        .items_center()
        .gap(px(7.))
        .surface(look, Surface::Capsule)
        .occlude()
        .text_xs()
        .on_hover(super::card::track_hover(
            id.clone(),
            |desk| &mut desk.controls_hover,
            cx,
        ))
        .child(identity(desk, placement, origin, look))
        .children(middle.map(|middle| {
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(7.))
                .child(rule(look))
                .child(middle)
        }))
        .child(rule(look))
        .child(actions(desk, &id, zoomed, offer.behind, cx))
        .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
            if let Some(tab) = desk.active_tab_id() {
                desk.focus_placement(&tab, &focus_id, window, cx);
            }
            if event.click_count() >= 2 {
                desk.execute("zoom", window, cx);
            }
        }))
}

/// Who runs in the pane: its harness mark, its name, and its agent's state
/// as a small lit chip.
fn identity(desk: &Desk, placement: &Placement, origin: PaneOrigin, look: &Look) -> gpui::Div {
    let agent = placement
        .terminal_id
        .as_ref()
        .and_then(|id| desk.agent(id))
        .cloned();
    let state = agent.as_ref().map(|info| info.state);
    let name = agent.as_ref().map_or_else(
        || desk.pane_title(placement.terminal_id.as_ref()),
        |info| info.name.clone(),
    );
    let word = agent
        .as_ref()
        .map(AgentInfo::word)
        .filter(|word| !word.is_empty());
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .children(
            origin
                .harness()
                .map(|harness| harness_mark(harness, state, 14., look)),
        )
        .child(
            div()
                .min_w_0()
                .max_w(px(220.))
                .truncate()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(look.ink)
                .child(name),
        )
        .children(word.zip(state).map(|(word, state)| {
            let swatch = from_hex(agent_swatch(state));
            div()
                .flex_shrink_0()
                .h(px(18.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(4.))
                .rounded_full()
                .bg(swatch.opacity(0.14))
                .text_color(swatch)
                .child(dot(5., swatch))
                .child(word)
        }))
}

/// A hairline between the capsule's groups.
fn rule(look: &Look) -> gpui::Div {
    div()
        .flex_shrink_0()
        .w(px(1.))
        .h(px(14.))
        .bg(look.ink.opacity(0.12))
}

/// The pane's view as two glyphs in one track, the shown one raised. Its
/// width never changes, so a click never moves the capsule under the pointer.
fn view_switch(desk: &Desk, id: &str, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let showing = desk.conversation_visible(id);
    let ink = look.ink;
    let chord = desk.shown_chord("conversation");
    let side = |conversation: bool| {
        let on = showing == conversation;
        let (icon, label, key) = if conversation {
            (IconName::MessagesSquare, "Conversation", "conversation")
        } else {
            (IconName::SquareTerminal, "Terminal", "terminal")
        };
        let tip = match &chord {
            Some(chord) if !on => format!("{label}  {chord}"),
            _ => label.to_owned(),
        };
        let toggle = id.to_owned();
        ghost_icon(format!("pane-{key}-{id}"), icon, tip)
            .toggled(on)
            .rounded_full()
            .bg(ink.opacity(if on { 0.12 } else { 0. }))
            .text_color(ink.opacity(if on { 1. } else { 0.55 }))
            .on_click(cx.listener(move |desk, _, window, cx| {
                cx.stop_propagation();
                if desk.conversation_visible(&toggle) != conversation {
                    desk.toggle_conversation(&toggle, window, cx);
                }
            }))
    };
    div()
        .debug_selector({
            let id = id.to_owned();
            move || format!("pane-conversation-{id}")
        })
        .flex_shrink_0()
        .flex()
        .gap(px(1.))
        .p(px(2.))
        .rounded_full()
        .bg(ink.opacity(0.05))
        .child(side(false))
        .child(side(true))
}

/// The one step between this agent and its native conversation: copy its
/// plugin install, or launch a harness that has none as an ACP agent. It
/// wears the accent, so it reads as an invitation rather than one more control.
fn setup_button(
    id: &str,
    harness: Harness,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let accent = from_hex(&look.chrome.theme.accent);
    let (label, tip) = match harness.plugin_install() {
        Some(command) => (
            "Set up conversation",
            format!(
                "Copies `{command}`\nRun it, then restart {} here; its conversation opens in this pane.",
                harness.label()
            ),
        ),
        None => (
            "Launch natively",
            format!(
                "{} has no phux plugin; start it as an ACP agent",
                harness.label()
            ),
        ),
    };
    let selector = format!("conversation-setup-{id}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex_shrink_0()
        .h(px(22.))
        .pl(px(7.))
        .pr(px(9.))
        .flex()
        .items_center()
        .gap(px(5.))
        .rounded_full()
        .border_1()
        .border_color(accent.opacity(0.35))
        .bg(linear_gradient(
            180.,
            linear_color_stop(accent.opacity(0.22), 0.),
            linear_color_stop(accent.opacity(0.1), 1.),
        ))
        .text_color(accent)
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(move |style| {
            style
                .bg(accent.opacity(0.26))
                .border_color(accent.opacity(0.6))
        })
        .active(move |style| style.bg(accent.opacity(0.34)))
        .child(
            Icon::new(IconName::MessagesSquare)
                .size(px(12.))
                .text_color(accent),
        )
        .child(label)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |desk, _, window, cx| {
            cx.stop_propagation();
            desk.offer_native_setup(harness, window, cx);
        }))
}

/// Split right, split down, zoom (naming what it hides while zoomed), and
/// the pane's menu, each tooltip carrying its chord.
fn actions(
    desk: &Desk,
    id: &str,
    zoomed: bool,
    behind: usize,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let tip = |label: &str, command: &str| match desk.shown_chord(command) {
        Some(chord) => format!("{label}  {chord}"),
        None => label.to_owned(),
    };
    let zoom_label = match (zoomed, behind) {
        (false, _) => "Zoom".to_owned(),
        (true, 0) => "Restore".to_owned(),
        (true, 1) => "Restore · 1 pane behind".to_owned(),
        (true, count) => format!("Restore · {count} panes behind"),
    };
    let button = |command: &'static str, icon: IconName, label: String| {
        let run = on_pane(id.to_owned(), command);
        ghost_icon(format!("{command}-{id}"), icon, label).on_click(cx.listener(
            move |desk, _, window, cx| {
                cx.stop_propagation();
                run(desk, window, cx);
            },
        ))
    };
    let menu_open = desk.pane_menu_open.as_deref() == Some(id);
    let (placement, selector) = (id.to_owned(), id.to_owned());
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(1.))
        .when(!zoomed, |row| {
            row.child(button(
                "split-right",
                IconName::PanelRight,
                tip("Split right", "split-right"),
            ))
            .child(button(
                "split-down",
                IconName::PanelBottom,
                tip("Split down", "split-down"),
            ))
        })
        .child(button(
            "zoom",
            if zoomed {
                IconName::Minimize
            } else {
                IconName::Maximize
            },
            tip(&zoom_label, "zoom"),
        ))
        .child(
            div()
                .relative()
                .debug_selector(move || format!("pane-actions-{selector}"))
                .child(desk.anchors.probe(format!("pane-actions-{id}")))
                .child(
                    ghost_icon(
                        format!("pane-more-{id}"),
                        IconName::Ellipsis,
                        "Pane actions",
                    )
                    .toggled(menu_open)
                    .on_click(cx.listener(
                        move |desk, event: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            let anchor = desk
                                .anchors
                                .get(&format!("pane-actions-{placement}"))
                                .map_or(Anchor::At(event.position()), Anchor::Below);
                            desk.open_pane_menu(&placement, anchor, window, cx);
                        },
                    )),
                ),
        )
}

/// Shown over the pane a file drag from the OS is above.
pub(super) fn drop_hint(look: &Look, remote: bool) -> impl IntoElement {
    let accent = from_hex(&look.chrome.theme.accent);
    let (title, detail) = if remote {
        (
            "Drop to upload",
            "Sent to that machine, then its path is pasted",
        )
    } else {
        ("Drop to attach", "Paths paste here, ready for your agent")
    };
    div()
        .absolute()
        .inset_0()
        .rounded(px(look.skin.shape.radius))
        .bg(accent.opacity(0.1))
        .border_2()
        .border_color(accent.opacity(0.8))
        .flex()
        .items_center()
        .justify_center()
        .child(
            notice_card(look, 0.92)
                .px(px(16.))
                .py(px(10.))
                .rounded(px(look.skin.shape.card_radius - 2.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(look.ink)
                        .child(title),
                )
                .child(muted(look, detail)),
        )
}

/// Focus this pane, then run `command` on it.
fn on_pane(
    placement: String,
    command: &'static str,
) -> impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static {
    move |desk, window, cx| {
        if let Some(tab) = desk.active_tab_id() {
            desk.focus_placement(&tab, &placement, window, cx);
        }
        desk.execute(command, window, cx);
    }
}
