//! Opening, moving, closing and finding desk windows, and the performance monitor.

use super::*;

impl Desktop {
    pub(crate) fn open_settings(
        &mut self,
        desk: Entity<Desk>,
        owner: AnyWindowHandle,
        page: Option<super::super::settings_panel::Page>,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if let Some((handle, settings)) = &self.settings {
            if settings.read(cx).owner == owner
                && handle
                    .update(cx, |_, window, cx| {
                        settings.update(cx, |settings, cx| settings.reveal(page, window, cx));
                        if crate::diag::may_activate() {
                            window.activate_window();
                        }
                    })
                    .is_ok()
            {
                return Ok(());
            }
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.settings = Some(super::super::settings_panel::window::open(
            desk, owner, page, cx,
        )?);
        Ok(())
    }

    /// Update reuse metadata without reading the desk while it is leased.
    pub(crate) fn sync_window_session(
        &mut self,
        desk: gpui::EntityId,
        name: &str,
        remote: Option<&str>,
        role: WindowRole,
    ) {
        if let Some(opened) = self
            .windows
            .iter_mut()
            .chain(self.quick.as_mut())
            .find(|opened| opened.desk.entity_id() == desk)
        {
            name.clone_into(&mut opened.session);
            opened.remote = remote.map(str::to_owned);
            opened.role = role;
        }
    }

    /// Any remaining window can keep the singleton home current after its own
    /// window closes. Once learned, the key prevents adopting a reused name.
    pub(crate) fn observe_topology(&mut self, topology: &phux_client_runtime::control::Topology) {
        let previous = self.home_key;
        crate::connection::sync_home_name(topology, &mut self.home_session, &mut self.home_key);
        if !self.explicit_layout
            && self.home_key != previous
            && let Some(key) = self.home_key
        {
            let paths = persist::session_layout_paths(&self.socket.display().to_string(), key);
            self.legacy_layout = Some(std::mem::replace(&mut self.layout, paths.layout));
            self.legacy_quick = Some(std::mem::replace(&mut self.quick_layout, paths.quick));
        }
    }

    pub fn open_moved(
        &mut self,
        session: String,
        terminal: ResourceId,
        remote: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.open_fresh(session, Some(terminal), remote, cx);
    }

    /// The next open window after `source` that talks to the same server
    /// (`remote`, `None` for local) and can take a moved terminal now. The
    /// quick window is never a target. The source desk is never read: the
    /// caller is usually updating it.
    pub(crate) fn next_window(
        &self,
        source: EntityId,
        remote: Option<&str>,
        cx: &App,
    ) -> Option<(AnyWindowHandle, Entity<Desk>)> {
        let peers: Vec<_> = self
            .windows
            .iter()
            .map(|opened| {
                let id = opened.desk.entity_id();
                let ready = id != source && opened.desk.read(cx).accepts_moved_terminal();
                (id, opened.remote.as_deref(), ready)
            })
            .collect();
        let index = next_peer(&peers, source, remote)?;
        let opened = &self.windows[index];
        Some((opened.handle, opened.desk.clone()))
    }

    /// Show registered host `name`: raise its window, or find the session to
    /// open there off the main thread and open one. `done` hears the refusal.
    pub fn open_host(
        &mut self,
        name: String,
        done: impl FnOnce(Result<(), String>, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        if let Some(opened) = self
            .windows
            .iter()
            .find(|opened| opened.remote.as_deref() == Some(name.as_str()))
            && opened
                .handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            cx.activate(true);
            done(Ok(()), cx);
            return;
        }
        let probe = name.clone();
        cx.spawn(async move |this, cx| {
            let home =
                smol::unblock(move || crate::connection::home::open_remote_home(&probe)).await;
            let _ = this.update(cx, |desktop, cx| {
                let result = home.and_then(|session| {
                    let paths = crate::workspace::layout_paths(&format!("host:{name}"), &session);
                    let request = OpenRequest {
                        session,
                        layout_path: paths.layout,
                        stable_layout: true,
                        remote: Some(name),
                        ..OpenRequest::default()
                    };
                    desktop
                        .open_window(request, true, false, cx)
                        .map(|_| ())
                        .map_err(|error| format!("{error:#}"))
                });
                done(result, cx);
            });
        })
        .detach();
    }

    /// A system notification was clicked: bring its window forward and open the
    /// entry. The tag is `<desk entity id>/<entry id>`, as [`Desk::flush_system`] posts it.
    pub(super) fn open_notification(&mut self, tag: &str, cx: &mut Context<Self>) {
        let Some(tag) = SystemTag::parse(tag) else {
            return;
        };
        let mut desks = self.opened();
        let found = match tag {
            SystemTag::Entry { desk, entry } => desks
                .find(|opened| opened.desk.entity_id().as_u64() == desk)
                .map(|opened| (opened, entry)),
            SystemTag::Approval(id) => desks.find_map(|opened| {
                let entry = opened.desk.read(cx).feed.approval_entry(id)?;
                Some((opened, entry))
            }),
        };
        let Some((opened, entry)) = found else {
            return;
        };
        let target = opened.desk.clone();
        cx.activate(true);
        let _ = opened.handle.update(cx, |_, window, cx| {
            window.activate_window();
            target.update(cx, |desk, cx| desk.open_notification(entry, window, cx));
        });
    }

    /// Every open terminal window in the order it opened, the quick terminal
    /// last. The performance window is a utility and is not listed.
    pub(crate) fn desk_windows(
        &self,
    ) -> impl Iterator<Item = (AnyWindowHandle, &Entity<Desk>, bool)> {
        self.windows
            .iter()
            .map(|opened| (opened.handle, &opened.desk, false))
            .chain(
                self.quick
                    .iter()
                    .map(|opened| (opened.handle, &opened.desk, true)),
            )
    }

    /// Whether a performance window currently owns the live sampler.
    pub(crate) fn performance_is_open(&self) -> bool {
        self.performance.is_some()
    }

    pub(crate) fn open_performance(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        if let Some(handle) = self.performance
            && handle
                .update(cx, |_, window, _| {
                    if crate::diag::may_activate() {
                        window.activate_window();
                    }
                })
                .is_ok()
        {
            return Ok(());
        }
        self.performance = Some(super::super::performance::open(cx)?);
        Ok(())
    }

    pub(crate) fn stop_performance(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.performance.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// Utility windows never count as workspace windows or trigger detach/quit.
    fn utility_window_closed(&mut self, id: WindowId, cx: &mut Context<Self>) -> bool {
        if let Some((handle, settings)) = &self.settings {
            if handle.window_id() == id {
                self.settings = None;
                return true;
            }
            if settings.read(cx).owner.window_id() == id {
                let (handle, _) = self.settings.take().expect("settings owner");
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
        }
        if self
            .performance
            .is_some_and(|handle| handle.window_id() == id)
        {
            self.performance = None;
            return true;
        }
        false
    }

    pub(super) fn window_closed(&mut self, id: WindowId, cx: &mut Context<Self>) {
        if self.utility_window_closed(id, cx) {
            return;
        }
        // A staged update installs on quit. Keep the final Desk alive until
        // the updater's quit observer snapshots it, before Desk releases views.
        if self.opened().count() == 1
            && self.updates.read(cx).staged.is_some()
            && self.opened().any(|opened| opened.handle.window_id() == id)
        {
            cx.quit();
            return;
        }
        let closed = if let Some(index) = self
            .windows
            .iter()
            .position(|opened| opened.handle.window_id() == id)
        {
            Some(self.windows.remove(index))
        } else if self
            .quick
            .as_ref()
            .is_some_and(|opened| opened.handle.window_id() == id)
        {
            self.quick.take()
        } else {
            None
        };
        let Some(opened) = closed else { return };
        opened.desk.update(cx, |desk, cx| desk.prepare_close(cx));
        if self.windows.is_empty() && self.quick.is_none() {
            cx.quit();
        }
    }

    pub(super) fn open_window(
        &mut self,
        request: OpenRequest,
        focus: bool,
        quick: bool,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<WindowHandle<Root>> {
        let desktop = cx.entity();
        let updates = self.updates.clone();
        let branding = self.branding.clone();
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let session = request.session.clone();
        let recent_projects = self.recent_projects().to_vec();
        let role = request.role;
        let remote = request.remote.clone();
        let bounds = if quick {
            size(px(1100.), px(460.))
        } else {
            size(px(1320.), px(840.))
        };
        let title: SharedString = if quick {
            "phux quick terminal".into()
        } else {
            "phux".into()
        };
        let (any, desk) = gpui_kit::open_window(
            WindowOptions {
                app_id: Some("dev.phux.desktop".into()),
                window_bounds: Some(WindowBounds::centered(bounds, cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title),
                    appears_transparent: true,
                    traffic_light_position: Some(Point::new(px(14.), px(14.))),
                }),
                focus,
                kind: crate::diag::window_kind(),
                show: true,
                window_min_size: Some(Size::new(px(640.), px(420.))),
                window_background: gpui::WindowBackgroundAppearance::Blurred,
                app_owns_titlebar_drag: true,
                ..WindowOptions::default()
            },
            cx,
            move |window, cx| {
                cx.new(|cx| {
                    Desk::open(
                        desktop,
                        updates,
                        branding,
                        phux,
                        socket,
                        request,
                        recent_projects,
                        window,
                        cx,
                    )
                })
            },
        )?;
        let handle = any
            .downcast::<Root>()
            .ok_or_else(|| anyhow::anyhow!("window root was not gpui_kit::component::Root"))?;
        let opened = Opened {
            handle: any,
            desk,
            session,
            role,
            remote,
        };
        if quick {
            self.quick = Some(opened);
        } else {
            self.windows.push(opened);
        }
        Ok(handle)
    }
}
