//! Local first-launch recovery and identity-safe reconnects.

use super::*;

/// Everything a probe is allowed to change. A later target or connection wins.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReconnectTarget {
    session: String,
    key: Option<SessionKey>,
    socket: PathBuf,
    phux: PathBuf,
    remote: Option<String>,
    connection: Option<ConnectionIdentity>,
    projection_epoch: u64,
}

impl ReconnectTarget {
    fn capture(desk: &Desk) -> Self {
        Self {
            session: desk.session.clone(),
            key: desk.link_home_key().or(desk.home_key),
            socket: desk.socket.clone(),
            phux: desk.phux.clone(),
            remote: desk.remote.clone(),
            connection: desk
                .link
                .as_ref()
                .and_then(|link| link.connection.identity()),
            projection_epoch: desk.upload_epoch,
        }
    }

    fn needs_rehome(&self) -> bool {
        self.session.is_empty() || self.socket.as_os_str().is_empty()
    }

    fn opens_home(&self) -> bool {
        // A learned key is an existing home even if a runtime target was lost.
        // Never recreate it by name after deletion or follow a reused name.
        self.key.is_none() && self.needs_rehome()
    }

    /// Runs entirely on the background executor: validation, server start,
    /// remote resolution, and the browsing connection can all block.
    fn resolve(&self, runtime_socket: Option<PathBuf>) -> Result<(String, PathBuf), String> {
        if let Some(remote) = self.remote.as_deref() {
            let name =
                crate::connection::home::existing_remote_home(remote, &self.session, self.key)?;
            return Ok((name, self.socket.clone()));
        }
        let socket = match runtime_socket {
            Some(socket) => socket,
            None => host::start_server(&self.phux, &self.socket)?,
        };
        let name = if self.opens_home() {
            let requested = (!self.session.is_empty()).then_some(self.session.as_str());
            crate::connection::home::open_home(&self.phux, &socket, requested)
        } else {
            crate::connection::home::existing_home(&socket, &self.session, self.key)
        }?;
        Ok((name, socket))
    }

    fn still_current(&self, desk: &Desk) -> bool {
        !desk.closed && !desk.session_is_attached() && *self == Self::capture(desk)
    }
}

impl Desk {
    /// An attached home is ready immediately. Otherwise queue one probe and
    /// return false; its completion performs the redial (or first-launch rehome).
    pub(super) fn prepare_reconnect_home(&mut self, cx: &mut Context<Self>) -> bool {
        if self.session_is_attached() {
            return true;
        }
        let target = ReconnectTarget::capture(self);
        let probe = target.clone();
        self.start_reconnect_home(target, async move { probe.resolve(None) }, cx);
        false
    }

    fn start_reconnect_home(
        &mut self,
        target: ReconnectTarget,
        probe: impl std::future::Future<Output = Result<(String, PathBuf), String>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.closed || self.reconnect_home.is_some() {
            return;
        }
        // Do not replace/cancel this task on another click: dropping a future
        // cannot stop a blocking subprocess and would allow workers to pile up.
        let resolved = cx.background_executor().spawn(probe);
        self.reconnect_home = Some(cx.spawn(async move |this, cx| {
            let result = resolved.await;
            let _ = this.update(cx, |desk, cx| {
                desk.reconnect_home = None;
                if target.still_current(desk) {
                    desk.finish_reconnect_home(target, result, cx);
                }
            });
        }));
    }

    fn finish_reconnect_home(
        &mut self,
        target: ReconnectTarget,
        result: Result<(String, PathBuf), String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            // A launch that could not name a session reopens on the one
            // the server now offers, with that session's saved layout.
            Ok((name, socket)) if target.needs_rehome() => {
                let old = cx.entity_id();
                let this = cx.entity().downgrade();
                self.defer_desktop(cx, move |desktop, cx| {
                    // Deferral is another async boundary: don't open a stale
                    // window if its source closed or changed in the meantime.
                    let Some(desk) = this.upgrade() else { return };
                    if target.still_current(desk.read(cx)) {
                        desktop.rehome(old, name, target.key, target.phux, socket, cx);
                    }
                });
            }
            Ok((name, socket)) => {
                // Adopt the target only once its home is known; a failed probe stays retryable.
                self.socket = socket;
                self.session = name.clone();
                self.workspace_call(|link| link.workspace.set_home_session(name));
                self.redial(cx);
            }
            Err(error) => {
                self.connect_error = Some(error.clone());
                self.post(Kind::Error, "Could not reconnect", Some(error));
                cx.notify();
            }
        }
    }

    pub(crate) fn reconnect(&mut self, cx: &mut Context<Self>) {
        if !self.prepare_reconnect_home(cx) {
            return;
        }
        self.redial(cx);
    }

    pub(super) fn runtime_reconnect(&mut self, socket: PathBuf, cx: &mut Context<Self>) {
        if self.remote.is_none() && self.socket.as_os_str().is_empty() {
            let target = ReconnectTarget::capture(self);
            let probe = target.clone();
            self.start_reconnect_home(target, async move { probe.resolve(Some(socket)) }, cx);
            return;
        }
        self.redial(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext};

    type HomeResult = Result<(String, PathBuf), String>;

    fn fixture(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, Entity<Desk>) {
        let (window, desk) = crate::app::overlays::palette_tests::fixture(cx);
        desk.update(cx, |desk, _| {
            desk.session = "work".into();
            desk.connect_error = Some("retry me".into());
        });
        (window, desk)
    }

    fn pending(desk: &Entity<Desk>, cx: &mut TestAppContext) -> async_channel::Sender<HomeResult> {
        let (send, receive) = async_channel::bounded(1);
        desk.update(cx, |desk, cx| {
            let target = ReconnectTarget::capture(desk);
            desk.start_reconnect_home(target, async move { receive.recv().await.unwrap() }, cx);
        });
        cx.run_until_parked();
        send
    }

    #[gpui::test]
    fn pending_probe_keeps_ui_responsive_and_retry_clicks_share_one_worker(
        cx: &mut TestAppContext,
    ) {
        let _dial = crate::connection::DeterministicDial::arm();
        let (_, desk) = fixture(cx);
        let result = pending(&desk, cx);
        desk.update(cx, |desk, cx| {
            for _ in 0..64 {
                assert!(!desk.prepare_reconnect_home(cx));
                let target = ReconnectTarget::capture(desk);
                desk.start_reconnect_home(
                    target,
                    async { panic!("a pending probe must not be replaced") },
                    cx,
                );
            }
            // Ordinary UI work must continue while the home probe is pending.
            desk.modal = Modal::Notifications;
            cx.notify();
        });
        cx.run_until_parked();
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.modal, Modal::Notifications);
            assert!(desk.reconnect_home.is_some());
            assert_eq!(desk.connect_error.as_deref(), Some("retry me"));
        });
        result
            .send_blocking(Ok(("renamed-work".into(), PathBuf::from("resolved.sock"))))
            .unwrap();
        cx.run_until_parked();
        desk.read_with(cx, |desk, _| {
            assert!(desk.reconnect_home.is_none());
            assert_eq!(desk.session, "renamed-work");
            assert_eq!(desk.socket, PathBuf::from("resolved.sock"));
            assert!(desk.link.is_some(), "success must continue the redial");
            assert!(desk.connect_error.is_none());
        });
    }

    #[gpui::test]
    fn failed_probe_preserves_target_and_allows_another_retry(cx: &mut TestAppContext) {
        let _dial = crate::connection::DeterministicDial::arm();
        let (_, desk) = fixture(cx);
        let before = desk.read_with(cx, |desk, _| ReconnectTarget::capture(desk));
        let result = pending(&desk, cx);
        result.send_blocking(Err("probe failed".into())).unwrap();
        cx.run_until_parked();
        desk.read_with(cx, |desk, _| {
            assert_eq!(ReconnectTarget::capture(desk), before);
            assert_eq!(desk.connect_error.as_deref(), Some("probe failed"));
            assert!(desk.reconnect_home.is_none());
            assert!(desk.link.is_none());
        });
        let retry = pending(&desk, cx);
        retry
            .send_blocking(Ok(("work".into(), before.socket)))
            .unwrap();
        cx.run_until_parked();
        desk.read_with(cx, |desk, _| assert!(desk.link.is_some()));
    }

    #[gpui::test]
    fn late_success_cannot_overwrite_any_changed_target_or_connection(cx: &mut TestAppContext) {
        let _dial = crate::connection::DeterministicDial::arm();
        let (_, desk) = fixture(cx);
        let changes: &[fn(&mut Desk)] = &[
            |desk| desk.session = "other".into(),
            |desk| desk.home_key = Some(SessionKey::new([7; 16]).unwrap()),
            |desk| desk.socket = PathBuf::from("other.sock"),
            |desk| desk.phux = PathBuf::from("other-phux"),
            |desk| desk.remote = Some("other-host".into()),
            |desk| desk.upload_epoch += 1,
            |desk| desk.closed = true,
        ];
        for change in changes {
            let result = pending(&desk, cx);
            desk.update(cx, |desk, _| change(desk));
            let changed = desk.read_with(cx, |desk, _| ReconnectTarget::capture(desk));
            result
                .send_blocking(Ok(("stale".into(), PathBuf::from("stale.sock"))))
                .unwrap();
            cx.run_until_parked();
            desk.read_with(cx, |desk, _| {
                assert_eq!(ReconnectTarget::capture(desk), changed);
                assert_eq!(desk.connect_error.as_deref(), Some("retry me"));
                assert!(desk.link.is_none());
                assert!(desk.reconnect_home.is_none());
            });
            desk.update(cx, |desk, _| desk.closed = false);
        }
    }

    #[gpui::test]
    fn late_failure_does_not_replace_a_changed_targets_error(cx: &mut TestAppContext) {
        let _dial = crate::connection::DeterministicDial::arm();
        let (_, desk) = fixture(cx);
        let result = pending(&desk, cx);
        desk.update(cx, |desk, _| {
            desk.remote = Some("new-host".into());
            desk.connect_error = Some("new target's error".into());
        });
        result.send_blocking(Err("stale error".into())).unwrap();
        cx.run_until_parked();
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.connect_error.as_deref(), Some("new target's error"));
            assert!(desk.reconnect_home.is_none());
        });
    }

    #[gpui::test]
    fn a_connection_that_attaches_while_probing_wins_over_the_late_result(cx: &mut TestAppContext) {
        let _dial = crate::connection::DeterministicDial::arm();
        let (_, desk) = fixture(cx);
        let result = pending(&desk, cx);
        desk.update(cx, |desk, _| {
            let mut connection =
                Connection::connect(desk.socket.clone(), desk.session.clone()).unwrap();
            connection.status = Status::Attached;
            desk.link = Some(Link {
                connection,
                workspace: Workspace::new("work".into(), WorkspaceOptions::default()),
            });
        });
        result
            .send_blocking(Err("stale probe error".into()))
            .unwrap();
        cx.run_until_parked();
        desk.read_with(cx, |desk, _| {
            assert!(desk.session_is_attached());
            assert_eq!(desk.connect_error.as_deref(), Some("retry me"));
        });
    }

    #[gpui::test]
    fn deferred_first_home_rechecks_identity_before_opening_a_window(cx: &mut TestAppContext) {
        let _dial = crate::connection::DeterministicDial::arm();
        let (window, desk) = fixture(cx);
        desk.update(cx, |desk, cx| {
            desk.session.clear();
            let target = ReconnectTarget::capture(desk);
            desk.finish_reconnect_home(target, Ok(("stale-home".into(), "stale.sock".into())), cx);
            desk.session = "new-home".into();
        });
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
        assert!(window.update(cx, |_, _, _| ()).is_ok());
        desk.read_with(cx, |desk, _| assert_eq!(desk.session, "new-home"));
    }

    #[gpui::test]
    fn successful_first_home_reopens_with_the_resolved_target(cx: &mut TestAppContext) {
        let _dial = crate::connection::DeterministicDial::arm();
        let (window, desk) = fixture(cx);
        desk.update(cx, |desk, _| {
            desk.session.clear();
            desk.socket.clear();
        });
        let result = pending(&desk, cx);
        desk.update(cx, |desk, cx| {
            for _ in 0..64 {
                // Runtime updates and Retry must share the first-home slot.
                desk.runtime_reconnect("unused-runtime-target".into(), cx);
                assert!(!desk.prepare_reconnect_home(cx));
            }
        });
        result
            .send_blocking(Ok(("first-home".into(), "resolved.sock".into())))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
        assert!(
            window.update(cx, |_, _, _| ()).is_err(),
            "old window was replaced"
        );
        let new_window = cx.update(|cx| cx.windows()[0]);
        let reopened = new_window
            .update(cx, |root, _, cx| {
                root.downcast::<gpui_kit::component::Root>()
                    .unwrap()
                    .read(cx)
                    .view()
                    .clone()
                    .downcast::<Desk>()
                    .unwrap()
            })
            .unwrap();
        reopened.read_with(cx, |desk, _| {
            assert_eq!(desk.session, "first-home");
            assert_eq!(desk.socket, PathBuf::from("resolved.sock"));
            assert!(desk.link.is_some());
        });
    }

    #[gpui::test]
    fn known_home_never_uses_the_create_capable_first_launch_path(cx: &mut TestAppContext) {
        let _dial = crate::connection::DeterministicDial::arm();
        let (_, desk) = fixture(cx);
        desk.update(cx, |desk, _| {
            assert!(!ReconnectTarget::capture(desk).opens_home());
            desk.socket.clear();
            assert!(ReconnectTarget::capture(desk).opens_home());
            desk.home_key = Some(SessionKey::new([7; 16]).unwrap());
            assert!(!ReconnectTarget::capture(desk).opens_home());
            desk.session.clear();
            assert!(!ReconnectTarget::capture(desk).opens_home());
        });
    }
}
