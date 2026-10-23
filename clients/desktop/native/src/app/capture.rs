//! Desktop launch spike. Durable cross-client intake is specified by ADR-0188;
//! this adapter deliberately uses the existing server spawn contract.

use gpui::{AppContext as _, Context, Entity, Subscription, Window};
use gpui_kit::component::input::{InputEvent, TextareaState};
use phux_client_runtime::control::{Event, PaneDescriptor, SpawnRequest, Topology};

use super::desk::{Desk, Modal};

mod composer;
pub(super) use composer::panel;

#[derive(Default)]
pub(crate) struct State {
    pub draft: String,
    pub error: Option<String>,
    pending: Option<Pending>,
    pub(super) editor: Option<(Entity<TextareaState>, Subscription)>,
}

struct Pending {
    id: u32,
    follow: bool,
    text: String,
    server_id: String,
    terminal: Option<phux_protocol::ResourceId>,
    client: std::sync::Arc<phux_client_runtime::Client>,
}

impl State {
    fn matches_draft(&self, text: &str, cx: &gpui::App) -> bool {
        self.draft == text
            && self
                .editor
                .as_ref()
                .is_none_or(|(input, _)| input.read(cx).value().as_str() == text)
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn cancel_pending(&mut self) {
        if self.pending.take().is_some() {
            self.error = Some("The connection changed. Delivery is unknown; check the original server's panes before retrying.".into());
        }
    }

    fn record_receipt(&mut self, events: &[Event]) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        for event in events {
            match event {
                Event::TerminalSpawned {
                    request_id,
                    terminal_id,
                    error,
                } if *request_id == pending.id => {
                    if terminal_id.is_none() || error.is_some() {
                        self.error = Some(
                            error
                                .clone()
                                .unwrap_or_else(|| "The server named no terminal.".into()),
                        );
                        self.pending = None;
                        return;
                    }
                    pending.terminal.clone_from(terminal_id);
                }
                Event::TerminalClosed { terminal_id, .. }
                | Event::TerminalAttached {
                    terminal_id,
                    error: Some(_),
                    ..
                } if pending.terminal.as_ref() == Some(terminal_id) => {
                    self.error = Some(format!(
                        "Capture launched {terminal_id}, but it could not be opened. Check that pane before retrying."
                    ));
                    self.pending = None;
                    return;
                }
                _ => {}
            }
        }
    }
}

/// An explicit @session selects its one known cwd; otherwise use focused cwd.
/// Never pick an arbitrary pane or use the desktop process's local directory.
pub(crate) fn resolve(
    text: &str,
    topology: &Topology,
    focused: Option<&PaneDescriptor>,
) -> Result<SpawnRequest, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Type a thought first.".into());
    }
    if text.len() > 16 * 1024 || text.contains('\0') {
        return Err("Capture accepts at most 16 KiB of text, without NUL characters.".into());
    }
    let (session_id, cwd, prompt) = if let Some(route) = text.strip_prefix('@') {
        explicit_route(route, topology)?
    } else {
        let (session, cwd) = focused_route(focused)?;
        (session, cwd, text)
    };
    if prompt.is_empty() {
        return Err("Type a thought after @session.".into());
    }
    Ok(SpawnRequest {
        command: Some(vec!["opencode".into(), "--prompt".into(), prompt.into()]),
        cwd: Some(cwd),
        session_id: Some(session_id),
        ..SpawnRequest::default()
    })
}

fn explicit_route<'a>(
    route: &'a str,
    topology: &Topology,
) -> Result<(u32, String, &'a str), String> {
    let (name, prompt) = route
        .split_once(char::is_whitespace)
        .ok_or("Use @session followed by a thought.")?;
    let session = topology
        .sessions
        .iter()
        .find(|session| session.name == name)
        .ok_or("That session is not on this server.")?;
    Ok((
        session.id,
        session_cwd(topology, session.id)?,
        prompt.trim(),
    ))
}

fn focused_route(focused: Option<&PaneDescriptor>) -> Result<(u32, String), String> {
    let pane = focused.ok_or("Focus a terminal with a known directory, or use @session.")?;
    if !matches!(pane.terminal_id, phux_protocol::ResourceId::Local { .. }) {
        return Err("Open a window on that machine before capturing there.".into());
    }
    let cwd = pane
        .cwd
        .clone()
        .filter(|cwd| !cwd.is_empty())
        .ok_or("The focused terminal has no known directory.")?;
    Ok((pane.session_id, cwd))
}

fn session_cwd(topology: &Topology, session: u32) -> Result<String, String> {
    let mut directories: Vec<&str> = topology
        .panes
        .iter()
        .filter(|pane| pane.session_id == session)
        .filter(|pane| matches!(pane.terminal_id, phux_protocol::ResourceId::Local { .. }))
        .filter_map(|pane| pane.cwd.as_deref())
        .filter(|cwd| !cwd.is_empty())
        .collect();
    directories.sort_unstable();
    directories.dedup();
    match directories.as_slice() {
        [cwd] => Ok((*cwd).into()),
        [] => Err("That session has no known directory.".into()),
        _ => Err("That session has multiple directories; focus the intended pane instead.".into()),
    }
}

impl Desk {
    pub(super) fn prepare_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match self.capture.editor.as_ref() {
            Some((input, _)) => input.clone(),
            None => {
                let input = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .placeholder("What needs doing?")
                        .default_value(self.capture.draft.clone())
                        .auto_grow(3, 8)
                        .submit_on_enter(true)
                });
                let subscription = cx.subscribe_in(
                    &input,
                    window,
                    |desk, input, event: &InputEvent, window, cx| {
                        if !matches!(
                            event,
                            InputEvent::Change | InputEvent::PressEnter { shift: false, .. }
                        ) {
                            return;
                        }
                        desk.capture.draft = input.read(cx).value().to_string();
                        if let InputEvent::PressEnter {
                            shift: false,
                            secondary,
                        } = event
                        {
                            desk.submit_capture(!secondary, window, cx);
                        }
                        cx.notify();
                    },
                );
                self.capture.editor = Some((input.clone(), subscription));
                input
            }
        };
        input.update(cx, |input, cx| {
            if input.value().as_str() != self.capture.draft {
                input.set_value(self.capture.draft.clone(), window, cx);
            }
            input.focus(window, cx);
        });
    }

    pub(super) fn submit_capture(
        &mut self,
        follow: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.capture.pending.is_some() {
            return;
        }
        let result = self.queue_capture(follow);
        match result {
            Ok(pending) => {
                self.capture.error = None;
                self.capture.pending = Some(pending);
            }
            Err(error) => self.capture.error = Some(error),
        }
        cx.notify();
    }

    fn queue_capture(&mut self, follow: bool) -> Result<Pending, String> {
        let link = self
            .link
            .as_ref()
            .ok_or("Not connected. Reconnect before dispatching.")?;
        let topology = link
            .connection
            .topology
            .as_ref()
            .ok_or("Waiting for server topology.")?;
        let request = resolve(&self.capture.draft, topology, self.focused_pane())?;
        let server_id = link
            .connection
            .server_id()
            .ok_or("Waiting for server identity.")?;
        let link = self.link.as_mut().expect("connected above");
        let id = link
            .workspace
            .launch_capture(&link.connection, request, follow)
            .map_err(|error| error.body)?;
        Ok(Pending {
            id,
            server_id,
            follow,
            text: self.capture.draft.clone(),
            terminal: None,
            client: std::sync::Arc::clone(&link.connection.client),
        })
    }

    pub(super) fn capture_receipts(&mut self, events: &[Event], cx: &mut Context<Self>) {
        let Some(pending) = self.capture.pending.as_ref() else {
            return;
        };
        if !self.capture_source_matches(pending) {
            self.capture.cancel_pending();
            return;
        }
        self.capture.record_receipt(events);
        if !self.capture_ready() {
            return;
        }
        let pending = self
            .capture
            .pending
            .take()
            .expect("matched pending capture");
        let desk = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = desk.update_in(cx, |desk, window, cx| {
                // Check at UI handoff, not before deferring: an edit or a new
                // submit in the meantime owns the field now.
                if !desk.capture_source_matches(&pending)
                    || !desk.capture.matches_draft(&pending.text, cx)
                    || desk.capture.is_pending()
                {
                    return;
                }
                desk.capture.draft.clear();
                if let Some((input, _)) = desk.capture.editor.as_ref() {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
                if desk.modal != Modal::Capture {
                    return;
                }
                if pending.follow {
                    desk.close_modal(window, cx);
                } else {
                    // Rebuild and focus the empty field, keeping the capture bar open.
                    desk.open_modal(Modal::Capture, window, cx);
                }
            });
        });
    }

    fn capture_ready(&self) -> bool {
        let Some(pending) = self.capture.pending.as_ref() else {
            return false;
        };
        let Some(terminal) = pending.terminal.as_ref() else {
            return false;
        };
        !pending.follow
            || self
                .link
                .as_ref()
                .is_some_and(|link| link.workspace.views_of(terminal) > 0)
    }

    fn capture_source_matches(&self, pending: &Pending) -> bool {
        self.link.as_ref().is_some_and(|link| {
            std::sync::Arc::ptr_eq(&link.connection.client, &pending.client)
                && link
                    .connection
                    .server_id()
                    .is_none_or(|server_id| server_id == pending.server_id)
        })
    }
}

#[cfg(test)]
mod tests;
