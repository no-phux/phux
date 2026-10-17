//! Explicit ACP launch UI. Only the CLI starts a durable server-owned host.
use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, div, px};
use gpui_kit::base::Disableable;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use phux_protocol::ResourceId;

use super::color::from_hex;
use super::desk::{Desk, Modal};
use super::host::{self, NativeIntegration};
use super::notifications::Kind;
use super::overlays::{card, corner};
use super::skin::Look;
use super::view::{ghost_button, muted};
use crate::connection::Connection;

#[derive(Default)]
pub(crate) struct State {
    integrations: Vec<NativeIntegration>,
    loading: bool,
    launching: Option<String>,
    error: Option<String>,
    pending: Option<Pending>,
}

struct Pending {
    owner: std::sync::Weak<phux_client_runtime::Client>,
    session: String,
    terminal: ResourceId,
    agent: ResourceId,
    activation: String,
    revealed: bool,
}

/// Whether `owner` still names `client`.
fn owned_by(
    owner: &std::sync::Weak<phux_client_runtime::Client>,
    client: &std::sync::Arc<phux_client_runtime::Client>,
) -> bool {
    std::ptr::eq(owner.as_ptr(), std::sync::Arc::as_ptr(client))
}

const LAUNCHED: &str = "The server-owned ACP session continues when this window closes. Waiting for its exact structured conversation.";

impl State {
    pub(super) fn owns_terminal(
        &self,
        client: &std::sync::Arc<phux_client_runtime::Client>,
        terminal: &ResourceId,
    ) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            &pending.terminal == terminal && owned_by(&pending.owner, client)
        })
    }
}

impl Desk {
    pub(crate) fn resume_native_conversation(
        &mut self,
        terminal: ResourceId,
        agent: ResourceId,
        activation: String,
    ) {
        if self.native_launch.pending.is_some() || self.native_launch.launching.is_some() {
            return;
        }
        let Some(link) = self.link.as_ref() else {
            return;
        };
        self.native_launch.pending = Some(Pending {
            owner: std::sync::Arc::downgrade(&link.connection.client),
            session: self.session.clone(),
            terminal,
            agent,
            activation,
            revealed: false,
        });
    }

    pub(crate) fn open_native_launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = Modal::NativeLaunch;
        if let Some(remote) = self.remote.as_deref() {
            self.native_launch.error = Some(format!(
                "This window is on {remote}. Native launch must run on that executing host: phux launch --acp INTEGRATION --json --session {}. The desktop never starts an agent locally for a remote session.",
                self.session
            ));
        } else {
            self.refresh_native_integrations(window, cx);
        }
    }

    fn refresh_native_integrations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.native_launch.loading || self.native_launch.launching.is_some() {
            return;
        }
        self.native_launch.loading = true;
        self.native_launch.error = None;
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || host::native_integrations(&phux, &socket)).await;
            let _ = this.update(cx, |desk, cx| {
                if desk.remote.is_some() {
                    return;
                }
                desk.native_launch.loading = false;
                match result {
                    Ok(items) => desk.native_launch.integrations = items,
                    Err(error) => desk.native_launch.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn launch_native_integration(
        &mut self,
        integration: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.native_launch.launching.is_some() || self.remote.is_some() {
            return;
        }
        if !self
            .native_launch
            .integrations
            .iter()
            .any(|item| item.integration == integration && item.native_available)
        {
            return;
        }
        let Some(owner) = self
            .link
            .as_ref()
            .filter(|link| link.connection.status == phux_client_runtime::control::Status::Attached)
            .map(|link| std::sync::Arc::downgrade(&link.connection.client))
        else {
            self.native_launch.error =
                Some("Reconnect to this session before launching a native agent.".into());
            cx.notify();
            return;
        };
        let session = self.session.clone();
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let integration = integration.to_owned();
        self.native_launch.launching = Some(integration.clone());
        self.native_launch.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let destination = session.clone();
            let result = smol::unblock(move || {
                host::launch_native(&phux, &socket, &integration, &destination)
            })
            .await;
            let _ = this.update_in(cx, |desk, window, cx| {
                if !desk
                    .link
                    .as_ref()
                    .is_some_and(|link| owned_by(&owner, &link.connection.client))
                {
                    return;
                }
                desk.native_launch.launching = None;
                match result {
                    Ok(launched) => {
                        desk.native_launch.pending = Some(Pending {
                            owner,
                            session,
                            terminal: ResourceId::local(launched.terminal_id),
                            agent: launched.agent_session_id,
                            activation: launched.activation,
                            revealed: false,
                        });
                        desk.close_modal(window, cx);
                        desk.reveal_pending_native(cx);
                        if let Some(link) = desk.link.as_ref() {
                            let _ = link.connection.refresh_topology();
                        }
                        desk.post(Kind::Info, "Native agent launched", Some(LAUNCHED.into()));
                    }
                    Err(error) => {
                        desk.native_launch.error = Some(error.clone());
                        if desk.modal != Modal::NativeLaunch {
                            desk.post(Kind::Error, "Native launch failed", Some(error));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn reveal_pending_native(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self
            .native_launch
            .pending
            .as_ref()
            .filter(|pending| !pending.revealed && pending_guardian_ready(self, pending))
        else {
            return;
        };
        let terminal = pending.terminal.clone();
        if let Some(pending) = self.native_launch.pending.as_mut() {
            pending.revealed = true;
        }
        self.reveal_terminal(&terminal);
        cx.notify();
    }

    pub(crate) fn prepare_pending_native(
        &mut self,
        placement: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.native_launch.pending.as_ref() else {
            return;
        };
        if !pending.revealed
            || !pending_matches(self, pending)
            || self.focused_placement().map(|item| item.id.as_str()) != Some(placement)
            || self
                .placement(placement)
                .and_then(|item| item.terminal_id.as_ref())
                != Some(&pending.terminal)
        {
            return;
        }
        self.prepare_conversation(placement, window, cx);
        if let Some(face) = self.conversation_faces.get_mut(placement) {
            face.visible = true;
            self.native_launch.pending = None;
        }
        self.focus_conversation(placement, window, cx);
    }
}

fn pending_connection<'a>(desk: &'a Desk, pending: &Pending) -> Option<&'a Connection> {
    let connection = &desk.link.as_ref()?.connection;
    (desk.session == pending.session && owned_by(&pending.owner, &connection.client))
        .then_some(connection)
}

fn pending_guardian_ready(desk: &Desk, pending: &Pending) -> bool {
    pending_connection(desk, pending).is_some_and(|connection| {
        // Opening the exact catalogued guardian establishes the agent subscription.
        // Requiring its not-yet-subscribed conversation here deadlocks new launches.
        connection.status == phux_client_runtime::control::Status::Attached
            && connection.pane(&pending.terminal).is_some()
            && connection.topology.as_ref().is_some_and(|topology| {
                topology.agent_sessions.iter().any(|session| {
                    session.id == pending.agent
                        && session.parent.as_ref() == Some(&pending.terminal)
                })
            })
    })
}

fn pending_matches(desk: &Desk, pending: &Pending) -> bool {
    pending_connection(desk, pending)
        .and_then(|connection| connection.conversation(&pending.terminal))
        .is_some_and(|conversation| {
            conversation.resource == pending.agent
                && conversation
                    .native
                    .as_ref()
                    .is_some_and(|view| view.activation == pending.activation)
        })
}

pub(crate) fn panel(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let colors = &look.chrome;
    let state = &desk.native_launch;
    let busy = state.loading || state.launching.is_some();
    let rows = state.integrations.iter().map(|item| {
        let integration = item.integration.clone();
        let title = item
            .display_name
            .as_deref()
            .unwrap_or(&item.integration)
            .to_owned();
        let launching = state.launching.as_deref() == Some(item.integration.as_str());
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(10.))
            .rounded(corner(look, 8.))
            .bg(from_hex(&colors.theme.background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(div().text_sm().child(title))
                    .child(
                        Button::new(SharedString::from(format!(
                            "launch-native-{}",
                            item.integration
                        )))
                        .label(if launching { "Launching…" } else { "Launch" })
                        .small()
                        .ghost()
                        .disabled(busy || !item.native_available || desk.remote.is_some())
                        .on_click(cx.listener(
                            move |desk, _, window, cx| {
                                desk.launch_native_integration(&integration, window, cx)
                            },
                        )),
                    ),
            )
            .children(
                item.native_detail
                    .as_ref()
                    .filter(|_| !item.native_available)
                    .map(|detail| muted(look, detail.clone())),
            )
    });
    card(look, 560.)
        .debug_selector(|| "native-launch-panel".into())
        .max_w_full()
        .max_h(px(620.))
        .p(px(18.))
        .rounded(corner(look, 12.))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(div().text_lg().child("New Agent"))
        .child(
            div()
                .id("native-launch-list")
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(8.))
                .children(rows),
        )
        .when(state.loading, |panel| panel.child("Loading agents…"))
        .when(!state.loading && state.integrations.is_empty(), |panel| {
            panel.child("No agents configured. Add an integration with phux, then refresh.")
        })
        .children(state.error.as_ref().map(|error| {
            div()
                .text_sm()
                .text_color(from_hex(&colors.theme.warning))
                .child(error.clone())
        }))
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(
                    ghost_button("native-launch-refresh", "Refresh")
                        .disabled(busy || desk.remote.is_some())
                        .on_click(cx.listener(|desk, _, window, cx| {
                            desk.refresh_native_integrations(window, cx)
                        })),
                )
                .child(
                    ghost_button("native-launch-close", "Close")
                        .on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx))),
                ),
        )
        .into_any_element()
}
