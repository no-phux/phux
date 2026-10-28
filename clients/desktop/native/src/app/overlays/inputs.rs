//! Modal text fields: building the open modal's controls, the find field,
//! and routing each field edit or Enter to what it submits.

use gpui::prelude::*;
use gpui::{App, Context, Entity, Subscription, Window};
use gpui_kit::component::command::CommandState;
use gpui_kit::component::input::{InputEvent, InputState};

use super::{InputSlot, PaletteView};
use crate::app::desk::{self, Desk, Modal};

/// Build the open modal's controls with fresh or prefilled values and give it the keyboard.
pub(crate) fn prepare(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) {
    match desk.modal.clone() {
        Modal::None => {}
        Modal::Capture => desk.prepare_capture(window, cx),
        Modal::AgentFleet => {
            desk.fleet_view.open();
            window.focus(&desk.focus, cx);
        }
        Modal::Commands
        | Modal::Goto
        | Modal::WindowSession
        | Modal::Sessions
        | Modal::Windows
        | Modal::MoveResources
        | Modal::Themes
        | Modal::Path => {
            desk.palette_view = PaletteView::default();
            let state = palette_state(desk, window, cx);
            state.update(cx, |state, cx| {
                state.set_loading(false, window, cx);
                state.set_query("", window, cx);
                state.focus(window, cx);
            });
        }
        Modal::Rename { tab_id } => {
            let current = desk
                .link
                .as_ref()
                .and_then(|link| link.workspace.tabs.iter().find(|tab| tab.id == tab_id))
                .map(|tab| desk::tab_title(desk, tab))
                .unwrap_or_default();
            let input = modal_input(
                desk,
                InputSlot::Rename,
                &current,
                "Follow the terminal title",
                window,
                cx,
            );
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        Modal::Session {
            create, current, ..
        } => {
            let name = modal_input(
                desk,
                InputSlot::SessionName,
                if create { "" } else { &current },
                "Session name",
                window,
                cx,
            );
            if create {
                let directory = desk.focused_cwd().unwrap_or_default();
                modal_input(
                    desk,
                    InputSlot::SessionDir,
                    &directory,
                    "Directory",
                    window,
                    cx,
                );
            }
            name.update(cx, |input, cx| input.focus(window, cx));
        }
        Modal::Project { current, .. } => {
            let input = modal_input(
                desk,
                InputSlot::Project,
                current.as_deref().unwrap_or_default(),
                "Project name",
                window,
                cx,
            );
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        Modal::AddHost => {
            let target = modal_input(
                desk,
                InputSlot::HostTarget,
                "",
                "ssh destination, e.g. me@mini",
                window,
                cx,
            );
            modal_input(
                desk,
                InputSlot::HostName,
                "",
                "Name (optional, defaults to the host)",
                window,
                cx,
            );
            target.update(cx, |input, cx| input.focus(window, cx));
            desk.prepare_add_machine(cx);
        }
        Modal::KillSession { .. }
        | Modal::NativeLaunch
        | Modal::Terminate { .. }
        | Modal::RemoveHost { .. }
        | Modal::Paste { .. }
        | Modal::Notifications => window.focus(&desk.focus, cx),
    }
}

pub(super) fn palette_state(
    desk: &mut Desk,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> Entity<CommandState> {
    if let Some(state) = desk.palette_state.clone() {
        return state;
    }
    let state = cx.new(|cx| CommandState::new(window, cx));
    desk.palette_state = Some(state.clone());
    state
}

pub(super) fn modal_input(
    desk: &mut Desk,
    slot: InputSlot,
    initial: &str,
    placeholder: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> Entity<InputState> {
    let (input, subscription) = input_with(slot, initial, placeholder, window, cx);
    desk.modal_inputs.push((slot, input.clone(), subscription));
    input
}

/// The find field, focused, reporting each edit and Enter / Shift-Enter.
pub(crate) fn find_input(
    initial: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> (Entity<InputState>, Subscription) {
    let (input, subscription) =
        input_with(InputSlot::Find, initial, "Find in terminal", window, cx);
    input.update(cx, |input, cx| input.focus(window, cx));
    (input, subscription)
}

pub(super) fn input_with(
    slot: InputSlot,
    initial: &str,
    placeholder: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> (Entity<InputState>, Subscription) {
    let initial = initial.to_owned();
    let placeholder = placeholder.to_owned();
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(initial)
    });
    let subscription = cx.subscribe_in(
        &input,
        window,
        move |desk, state, event: &InputEvent, window, cx| {
            let value = state.read(cx).value().to_string();
            match event {
                InputEvent::Change => desk.on_input(slot, value, None, window, cx),
                InputEvent::PressEnter { shift, .. } => {
                    desk.on_input(slot, value, Some(*shift), window, cx)
                }
                _ => {}
            }
        },
    );
    (input, subscription)
}

impl Desk {
    pub(crate) fn input_entity(&self, slot: InputSlot) -> Option<Entity<InputState>> {
        self.modal_inputs
            .iter()
            .find(|(owned, _, _)| *owned == slot)
            .map(|(_, input, _)| input.clone())
    }

    pub(super) fn input_value(&self, slot: InputSlot, cx: &App) -> String {
        self.input_entity(slot)
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// A field edit (`submit` is `None`) or Enter (`Some(shift)`).
    pub(super) fn on_input(
        &mut self,
        slot: InputSlot,
        value: String,
        submit: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (slot, submit) {
            (InputSlot::Find, None) => {
                let case = self.find.as_ref().is_some_and(|find| find.case_sensitive);
                self.run_search(value, case, cx);
            }
            (InputSlot::Find, Some(shift)) => {
                self.step_find(if shift { -1 } else { 1 }, window, cx)
            }
            (InputSlot::Rename, Some(_)) => self.apply_rename(value, window, cx),
            (InputSlot::SessionName | InputSlot::SessionDir, Some(_)) => {
                let name = self.input_value(InputSlot::SessionName, cx);
                let directory = self
                    .input_entity(InputSlot::SessionDir)
                    .map(|input| input.read(cx).value().to_string());
                self.apply_session(name, directory, window, cx);
            }
            (InputSlot::Project, Some(_)) => self.link_project(Some(value), window, cx),
            (InputSlot::HostTarget | InputSlot::HostName, Some(_)) => {
                let target = self.input_value(InputSlot::HostTarget, cx);
                let name = self.input_value(InputSlot::HostName, cx);
                self.apply_add_host(target, name, window, cx);
            }
            _ => {}
        }
        cx.notify();
    }
}
