//! Capture composes Kit's editor and input-group primitives, not a second text engine.

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, IntoElement, Window, div, px};
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputGroup, InputGroupAddon, InputGroupAddonAlignment, Textarea};
use gpui_kit::component::{IconName, Sizable};

use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::overlays::{SHEET_GUTTER, corner, sheet, sheet_top};
use crate::app::skin::Look;

// Ask the launch resolver about a destination even before the thought is
// complete. Never advertise a cwd that dispatch would refuse or route elsewhere.
fn destination(desk: &Desk) -> Result<String, String> {
    let link = desk.link.as_ref().ok_or("Not connected")?;
    let topology = link
        .connection
        .topology
        .as_ref()
        .ok_or("Waiting for server topology")?;
    let prompt = match desk
        .capture
        .draft
        .split_whitespace()
        .next()
        .filter(|word| word.starts_with('@'))
    {
        Some(route) => format!("{route} _"),
        None => "_".to_owned(),
    };
    let request = super::resolve(&prompt, topology, desk.focused_pane())?;
    Ok(request.cwd.expect("capture resolver requires a cwd"))
}

pub(in crate::app) fn panel(
    desk: &Desk,
    look: &Look,
    window: &Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some((input, _)) = desk.capture.editor.as_ref() else {
        return div().into_any_element();
    };
    let pending = desk.capture.is_pending();
    let muted = from_hex(&look.chrome.muted);
    let destination = destination(desk).unwrap_or_else(|error| error);
    let viewport = window.viewport_size();
    let height = (viewport.height.as_f32() - sheet_top(viewport) - SHEET_GUTTER).max(0.);
    let stay = if cfg!(target_os = "macos") {
        "⌘Enter"
    } else {
        "Ctrl+Enter"
    };
    sheet(look, 600.)
        .id("capture-panel")
        .debug_selector(|| "capture-panel".into())
        .max_h(px(height))
        .overflow_y_scroll()
        // Kit propagates a submitted Enter so its host can consume it. Do not
        // let an unhandled submit fall through to native newline insertion.
        .on_action(|_: &gpui_kit::base::input::Enter, _, _| {})
        .text_left()
        .p(px(16.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(
            div().flex().items_center().justify_between()
                .child(div().flex().items_center().gap(px(8.))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("New thought"))
                    .child(div().text_size(px(11.)).text_color(muted).child("OpenCode")))
                .child(Button::new("capture-close").icon(IconName::Close).ghost().xsmall()
                    .accessibility_label("Dismiss capture; keep draft")
                    .tooltip("Esc · keep draft")
                    .on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx)))),
        )
        .child(
            InputGroup::new("capture-composer")
                .aria_label("Capture a thought")
                .rounded(corner(look, 10.))
                .invalid(desk.capture.error.is_some())
                .input(Textarea::new(input).aria_label("Thought").text_size(px(15.)).p(px(12.)))
                .addon(InputGroupAddon::new("capture-toolbar")
                    .align(InputGroupAddonAlignment::BlockEnd)
                    .justify_between().gap(px(12.))
                    .child(div().id("capture-destination").flex_1().min_w_0().truncate()
                        .text_size(px(11.)).font_weight(FontWeight::NORMAL)
                        .child(destination))
                    .child(Button::new("capture-send").label(if pending { "Sending…" } else { "Send" })
                        .icon(IconName::ArrowUp).primary().small()
                        .disabled(pending || desk.capture.draft.trim().is_empty())
                        .accessibility_label("Send thought and follow agent")
                        .on_click(cx.listener(|desk, _, window, cx| desk.submit_capture(true, window, cx))))),
        )
        .child(div().flex().flex_wrap().gap_x(px(16.)).gap_y(px(4.))
            .text_size(px(11.)).text_color(muted)
            .child("Enter send & follow")
            .child("Shift+Enter new line")
            .child(format!("{stay} send & stay")))
        .children(desk.capture.error.clone().map(|error| {
            div().id("capture-error").role(gpui::Role::Alert)
                .text_size(px(12.)).text_color(from_hex(&look.chrome.theme.danger)).child(error)
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn preview_uses_launch_rules_instead_of_advertising_refused_destinations(
        cx: &mut gpui::TestAppContext,
    ) {
        let _dial = crate::connection::DeterministicDial::arm();
        let (_, desk) = crate::app::overlays::palette_tests::fixture(cx);
        let mut connection = crate::workspace::view_tests::attached();
        let workspace = crate::workspace::view_tests::opened(&mut connection);
        let topology = connection.topology.as_mut().unwrap();
        topology.panes[0].cwd = Some("/repo/project".into());
        let name = topology.sessions[0].name.clone();
        desk.update(cx, |desk, _| {
            desk.link = Some(crate::app::desk::Link {
                connection,
                workspace,
            });
            desk.capture.draft = format!("@{name}");
            assert_eq!(destination(desk).unwrap(), "/repo/project");
            desk.capture.draft = "@missing thought".into();
            assert!(
                destination(desk)
                    .unwrap_err()
                    .contains("not on this server")
            );
            desk.capture.draft = format!("@{name} thought");
            let topology = desk
                .link
                .as_mut()
                .unwrap()
                .connection
                .topology
                .as_mut()
                .unwrap();
            let mut other = topology.panes[0].clone();
            other.terminal_id = phux_protocol::ResourceId::local(999);
            other.cwd = Some("/repo/other".into());
            topology.panes.push(other);
            assert!(
                destination(desk)
                    .unwrap_err()
                    .contains("multiple directories")
            );
            let topology = desk
                .link
                .as_mut()
                .unwrap()
                .connection
                .topology
                .as_mut()
                .unwrap();
            topology.panes.truncate(1);
            topology.panes[0].terminal_id = phux_protocol::ResourceId::satellite("remote", 7);
            assert!(
                destination(desk).is_err(),
                "a remote cwd is not a local destination"
            );
        });
    }
}
