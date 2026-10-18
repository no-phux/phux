//! Desktop launch spike. Durable cross-client intake is specified by ADR-0187;
//! this adapter deliberately uses the existing server spawn contract.

use gpui::{Context, Window};
use phux_client_runtime::control::{Event, PaneDescriptor, SpawnRequest, Topology};

use super::desk::{Desk, Modal};

#[derive(Default)]
pub(crate) struct State {
    pub draft: String,
    pub error: Option<String>,
    pending: Option<Pending>,
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
                    || desk.capture.draft != pending.text
                    || desk.capture.is_pending()
                {
                    return;
                }
                desk.capture.draft.clear();
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
mod tests {
    use super::*;
    use phux_protocol::{
        ResourceId,
        ids::{SessionId, WindowId},
        wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
    };

    fn topology() -> Topology {
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(7))
                .with_sessions(vec![SessionInfo::new(SessionId::new(1), "project")])
                .with_windows(vec![WindowInfo::new(
                    WindowId::new(1),
                    SessionId::new(1),
                    "shell",
                )])
                .with_resources(vec![ResourceInfo::new(
                    ResourceId::local(7),
                    WindowId::new(1),
                    80,
                    24,
                )]);
        let mut topology = Topology::from_snapshot(&snapshot);
        topology.panes[0].cwd = Some("/repo/worktree".into());
        topology
    }

    fn pending_capture(follow: bool) -> State {
        let connection =
            crate::connection::Connection::connect("/capture-test.sock".into(), "test".into())
                .unwrap();
        State {
            draft: "original thought".into(),
            error: None,
            pending: Some(Pending {
                id: 42,
                follow,
                text: "original thought".into(),
                server_id: "original-server".into(),
                terminal: None,
                client: connection.client,
            }),
        }
    }

    fn spawned(error: Option<&str>) -> Event {
        Event::TerminalSpawned {
            request_id: 42,
            terminal_id: error.is_none().then(|| ResourceId::local(8)),
            error: error.map(str::to_owned),
        }
    }

    #[gpui::test]
    fn enter_keeps_the_capture_keyboard_until_a_later_bootstrap_places_the_agent(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::Focusable as _;
        use phux_protocol::{
            caps::BootstrapStreamProfile,
            ids::{BootstrapId, StreamId},
            wire::frame::{FrameKind, SpawnResult},
        };
        let _dial = crate::connection::DeterministicDial::arm();
        let (handle, desk) = crate::app::overlays::palette_tests::fixture(cx);
        let mut connection = crate::workspace::view_tests::attached();
        let workspace = crate::workspace::view_tests::opened(&mut connection);
        let cx = &mut gpui::VisualTestContext::from_window(handle, cx);
        desk.update(cx, |desk, _| {
            desk.link = Some(crate::app::desk::Link {
                connection,
                workspace,
            });
            desk.capture.draft = "first prompt".into();
        });
        cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("capture", window, cx)));
        let terminal = ResourceId::local(9);
        desk.update(cx, |desk, cx| {
            let link = desk.link.as_mut().unwrap();
            let id = link
                .workspace
                .launch_capture(&link.connection, SpawnRequest::default(), true)
                .unwrap();
            desk.capture.pending = Some(Pending {
                id,
                follow: true,
                text: desk.capture.draft.clone(),
                server_id: link.connection.server_id().unwrap(),
                terminal: None,
                client: std::sync::Arc::clone(&link.connection.client),
            });
            link.connection
                .client
                .feed(FrameKind::ResourceSpawned {
                    request_id: id,
                    result: SpawnResult::Ok(terminal.clone()),
                })
                .unwrap();
            let events = link.connection.drain();
            link.workspace.handle(&events, &link.connection);
            link.workspace.settle(&link.connection);
            desk.capture_receipts(&events, cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            desk.read_with(cx, |desk, cx| {
                assert_eq!(desk.modal, Modal::Capture);
                assert_eq!(desk.capture.draft, "first prompt");
                let (_, input, _) = desk.modal_inputs.first().unwrap();
                assert!(
                    input.read(cx).focus_handle(cx).is_focused(window),
                    "a spawn receipt must not return keys to the original shell"
                );
            });
        });
        desk.update(cx, |desk, cx| {
            let link = desk.link.as_mut().unwrap();
            let stream_id = StreamId::new(2).unwrap();
            let bootstrap_id = BootstrapId::new(2).unwrap();
            for frame in [
                FrameKind::BootstrapBegin {
                    terminal_id: terminal.clone(),
                    stream_id,
                    bootstrap_id,
                    profile: BootstrapStreamProfile::SynthesizedVtRaw,
                    cols: 80,
                    rows: 24,
                    base_seq: 0,
                },
                FrameKind::BootstrapChunk {
                    terminal_id: terminal.clone(),
                    stream_id,
                    bootstrap_id,
                    chunk_seq: 0,
                    payload: b"agent ready".to_vec().into(),
                },
                FrameKind::BootstrapReady {
                    terminal_id: terminal.clone(),
                    stream_id,
                    bootstrap_id,
                    history_cursor: None,
                },
            ] {
                link.connection.client.feed(frame).unwrap();
            }
            let events = link.connection.drain();
            link.workspace.handle(&events, &link.connection);
            link.workspace.settle(&link.connection);
            desk.capture_receipts(&events, cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.modal, Modal::None);
            assert!(desk.capture.draft.is_empty());
            assert_eq!(desk.focused_terminal(), Some(terminal));
        });
        // Verify the actual terminal focus handle, not just workspace selection.
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
        }
        cx.update(|window, cx| {
            desk.update(cx, |desk, cx| {
                let placement = desk.focused_placement().unwrap().id.clone();
                let terminal = desk
                    .panes_mut()
                    .get(&placement)
                    .expect("rendered agent terminal");
                assert!(
                    terminal.read(cx).focus_handle().is_focused(window),
                    "the ready agent must own the GPUI keyboard"
                );
            });
        });
    }

    #[test]
    fn receipt_keeps_follow_pending_until_placement_and_never_erases_in_flight_edits() {
        let _dial = crate::connection::DeterministicDial::arm();
        for follow in [false, true] {
            let mut state = pending_capture(follow);
            state.draft = "edited while awaiting receipt".into();
            state.record_receipt(&[spawned(None)]);
            assert_eq!(
                state.pending.as_ref().unwrap().terminal,
                Some(ResourceId::local(8))
            );
            assert_eq!(state.draft, "edited while awaiting receipt");
            assert!(
                state.is_pending(),
                "a spawn receipt alone must not hand keyboard focus back"
            );
        }
    }

    #[test]
    fn asynchronous_refusal_and_connection_replacement_retain_the_draft() {
        let _dial = crate::connection::DeterministicDial::arm();
        let mut state = pending_capture(true);
        state.record_receipt(&[spawned(Some("spawn refused"))]);
        assert!(!state.is_pending());
        assert_eq!(state.error.as_deref(), Some("spawn refused"));
        assert_eq!(state.draft, "original thought");

        let mut state = pending_capture(false);
        state.cancel_pending();
        state.record_receipt(&[spawned(None)]); // reused id from a replacement client
        assert!(!state.is_pending());
        assert_eq!(state.draft, "original thought");
        assert!(state.error.unwrap().contains("Delivery is unknown"));
    }

    #[test]
    fn focused_route_preserves_unicode_and_shell_characters_as_one_argument() {
        let topology = topology();
        let text = "fix 東京; $(touch /bad) 'quote'\nsecond line";
        let request = resolve(text, &topology, topology.panes.first()).unwrap();
        assert_eq!(request.cwd.as_deref(), Some("/repo/worktree"));
        assert_eq!(request.session_id, Some(1));
        assert_eq!(request.command.unwrap(), vec!["opencode", "--prompt", text]);
    }

    #[test]
    fn explicit_route_does_not_need_focus_but_refuses_ambiguous_worktrees() {
        let mut topology = topology();
        assert_eq!(
            resolve("@project fix it", &topology, None)
                .unwrap()
                .command
                .unwrap()[2],
            "fix it"
        );
        let mut second = topology.panes[0].clone();
        second.terminal_id = ResourceId::local(8);
        topology.panes.push(second);
        assert!(
            resolve("@project fix it", &topology, None).is_ok(),
            "same cwd is unambiguous"
        );
        topology.panes[1].cwd = Some("/repo/other-worktree".into());
        assert!(
            resolve("@project fix it", &topology, None)
                .unwrap_err()
                .contains("multiple directories")
        );
    }

    #[test]
    fn missing_destinations_or_empty_prompts_never_fall_back_to_another_directory() {
        let mut topology = topology();
        for text in ["", "  ", "@missing fix it", "@project", "@project   "] {
            assert!(resolve(text, &topology, None).is_err(), "{text:?}");
        }
        assert!(resolve("fix it", &topology, None).is_err());
        topology.panes[0].cwd = None;
        assert!(resolve("fix it", &topology, topology.panes.first()).is_err());
        assert!(resolve("@project fix it", &topology, None).is_err());
    }

    #[test]
    fn bounds_and_remote_paths_fail_closed() {
        let mut topology = topology();
        for text in ["x\0y".to_owned(), "é".repeat(8193)] {
            assert!(resolve(&text, &topology, topology.panes.first()).is_err());
        }
        assert!(resolve(&"é".repeat(8192), &topology, topology.panes.first()).is_ok());
        topology.panes[0].terminal_id = ResourceId::satellite("remote", 7);
        assert!(resolve("fix it", &topology, topology.panes.first()).is_err());
        assert!(resolve("@project fix it", &topology, None).is_err());
    }
}
