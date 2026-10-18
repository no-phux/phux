//! Buttons that carry their own accessible name and tooltip.

use gpui::prelude::*;
use gpui::{App, ClickEvent, IntoElement, MouseButton, SharedString, Window, div};
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};

/// A small ghost icon button whose `label` names it and is its tooltip.
pub(crate) fn ghost_icon(
    id: impl Into<gpui::ElementId>,
    icon: impl Into<gpui_kit::component::Icon>,
    label: impl Into<SharedString>,
) -> Button {
    let label = label.into();
    Button::new(id)
        .icon(icon.into())
        .ghost()
        .xsmall()
        .accessibility_label(label.clone())
        .tooltip(label)
}

/// A small ghost button showing `label`.
pub(crate) fn ghost_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> Button {
    Button::new(id).label(label).ghost().small()
}

/// A chrome icon button that keeps the titlebar from dragging the window when
/// pressed. It stops the press rather than occluding: an occluder would hide
/// the titlebar's own hover, and the slim strip reveals its skin toggle on
/// that hover, so the toggle would vanish as the pointer reached it.
pub(crate) fn icon_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    chord: Option<String>,
    click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let button = ghost_icon(id, icon, label).small().on_click(click);
    div()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(match chord {
            Some(chord) => button.tooltip(format!("{label}  {chord}")),
            None => button,
        })
}
