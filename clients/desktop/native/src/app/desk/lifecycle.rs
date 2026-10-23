//! Opening a window, attaching its connection, and reconnecting or closing it.

use super::*;

impl Desk {
    #[allow(
        clippy::too_many_arguments,
        reason = "window construction binds desktop handles, request, and its navigation snapshot"
    )]
    pub(crate) fn open(
        desktop: Entity<Desktop>,
        updates: Entity<super::super::updates::Updater>,
        phux: PathBuf,
        socket: PathBuf,
        request: OpenRequest,
        recent_projects: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store =
            FileLayoutStore::new(request.layout_path.clone(), request.legacy_layout.clone());
        let loaded = persist::load_layout(&store.read());
        let blocked = loaded.blocked;
        let resuming = request.resume_layout.is_some();
        let saved = request.resume_layout.clone().or(loaded.initial);
        let text = settings::read_ghostty_sources();
        let ghostty = settings::parse_ghostty_sources(text.as_deref().unwrap_or_default());
        let mut prefs = saved
            .as_ref()
            .map(|layout| layout.display.clone())
            .unwrap_or_else(|| {
                if text.is_some() {
                    settings::ghostty_prefs(&settings::default_display(), &ghostty)
                } else {
                    settings::default_display()
                }
            });
        if request.role == WindowRole::Quick {
            prefs = persist::quick_display(
                request.main_display.as_ref().or(Some(&prefs)),
                saved.as_ref(),
            );
        }
        prefs = settings::sanitize_display(&prefs);
        let keymap = settings::effective_keymap(&ghostty, prefs.ghostty_keys);
        let skin_target = resolved_skin(&prefs);
        let focus = cx.focus_handle();
        let saves = persist::writer::Writer::default();
        let errors = saves.errors();
        let save_errors = cx.spawn(async move |this, cx| {
            while let Ok(error) = errors.recv().await {
                let _ = this.update(cx, |desk, cx| {
                    desk.post(Kind::Error, "Could not save desktop layout", Some(error));
                    cx.notify();
                });
            }
        });
        let mut desk = Self {
            desktop,
            updates,
            role: request.role,
            socket,
            remote: request.remote.clone(),
            session: request.session.clone(),
            home_key: request.home_key,
            expected_authority: request.authority,
            phux,
            focus,
            prefs,
            ghostty_found: text.is_some(),
            chords: shown_chords(&keymap),
            keymap,
            ghostty,
            paint: RefCell::new(None),
            capture: super::super::capture::State::default(),
            changes: Default::default(),
            modal: Modal::None,
            prepared: Modal::None,
            find: None,
            picker: None,
            feed: Feed::default(),
            recent_projects,
            link: None,
            connect_error: None,
            layout_blocked: blocked,
            persist_writes: request.role != WindowRole::Fresh && !blocked,
            store,
            locations: HashMap::new(),
            saves,
            _save_errors: save_errors,
            stable_layout: request.stable_layout,
            // The handoff already names this store and session. Adopting it
            // again would replace the restart snapshot with older disk state.
            layout_key: request.home_key.filter(|_| resuming),
            layout_migration_pending: false,
            last_saved: saved,
            panes: Panes::default(),
            tab_peek: Default::default(),
            agent_seen: HashMap::new(),
            agent_epoch: None,
            commands: Default::default(),
            approvals: Default::default(),
            drag: None,
            split_bounds: Rc::default(),
            tab_geometry: Rc::default(),
            viewport: gpui::size(gpui::px(1320.), gpui::px(840.)),
            _wake: None,
            runtime_chrome: RuntimeChrome::default(),
            _clock: None,
            clock_at: None,
            _subs: Vec::new(),
            palette_state: None,
            palette_view: Default::default(),
            goto: GotoPalette::default(),
            goto_search: GotoSearch::default(),
            goto_frecency: GotoFrecency::default(),
            modal_inputs: Vec::new(),
            find_input: None,
            conversation_faces: HashMap::new(),
            settings_controls: None,
            native_launch: Default::default(),
            open_fresh: request.fresh && !resuming,
            pending_project_terminal: None,
            expanded: Default::default(),
            disclosures: HashMap::new(),
            hosts: Vec::new(),
            fleet: Default::default(),
            fleet_view: Default::default(),
            add_machine: AddMachine::default(),
            drag_region: None,
            menu: None,
            menu_closed: None,
            anchors: Default::default(),
            chrome: super::super::view::ChromeCache::default(),
            window_title: String::new(),
            closed: false,
            creates: HashMap::new(),
            renames: HashMap::new(),
            kills: HashMap::new(),
            organization: Default::default(),
            motion: Motion::default(),
            skin_target,
            shift: None,
            backdrop: None,
            chrome_hover: None,
            controls_hover: None,
            pane_menu_open: None,
            drop_target: None,
            uploads: Vec::new(),
            upload_epoch: 0,
            uploads_reading: 0,
        };
        if let Some(error) = request.startup_error {
            desk.post(Kind::Error, "Could not start the phux server", Some(error));
        }
        if blocked {
            desk.post(
                Kind::Error,
                "Saved layout preserved",
                Some("This layout is invalid or from an unsupported version. Layout and settings changes will not be saved.".into()),
            );
        }
        desk.attach(request.initial_terminal, cx);
        desk.refresh_hosts(cx);
        desk.sync_bindings(cx);
        desk.set_motion_spans();
        desk.drive_skin(window, cx);
        desk.apply_kit_theme(cx);
        let updates = desk.updates.clone();
        let mut runtime_generation = updates.read(cx).runtime_generation;
        desk._subs.push(cx.observe(&updates, move |_, updates, cx| {
            let generation = updates.read(cx).runtime_generation;
            if generation != runtime_generation {
                runtime_generation = generation;
                cx.spawn(async move |this, cx| {
                    smol::Timer::after(Duration::from_millis(500)).await;
                    let _ = this.update(cx, |desk, cx| desk.redial(cx));
                })
                .detach();
            }
            cx.notify();
        }));
        // Quit saves every window; a closed window saves through `Desktop::window_closed`.
        desk._subs.push(cx.on_app_quit(|desk, cx| {
            desk.prepare_close(cx);
            async {}
        }));
        // Window keys need the `Desk` context on the dispatch path before any terminal exists.
        window.focus(&desk.focus, cx);
        desk
    }

    pub(super) fn attach(&mut self, initial: Option<ResourceId>, cx: &mut Context<Self>) {
        // Native launch completions belong to the client that initiated them.
        self.reset_native_launch();
        if self.session.is_empty() {
            // The launch could not name a session; its reason is already shown.
            self.connect_error = Some("No session to open".into());
            cx.notify();
            return;
        }
        if self.resume_link(cx) {
            return;
        }
        let connected = match self.remote.as_deref() {
            Some(host) => {
                Connection::connect_remote_with_key(host, self.session.clone(), self.home_key)
            }
            None => Connection::connect_with_key(
                self.socket.clone(),
                self.session.clone(),
                self.home_key,
            ),
        };
        match connected {
            Ok(mut connection) => {
                connection.expect_authority(self.expected_authority);
                let mut workspace = Workspace::new(
                    self.session.clone(),
                    WorkspaceOptions {
                        fresh: self.open_fresh,
                        initial_terminal: initial,
                    },
                );
                if self.home_key.is_some() {
                    workspace.preserve_empty();
                }
                // A secondary window shows new terminals; every other window restores its own layout.
                if self.role != WindowRole::Fresh || !self.open_fresh {
                    workspace.restore(self.last_saved.clone());
                }
                self.arm_wake(&connection, cx);
                self.link = Some(Link {
                    connection,
                    workspace,
                });
                self.connect_error = None;
            }
            Err(error) => {
                self.connect_error = Some(error.to_string());
                self.post(Kind::Error, "Could not connect", Some(error.to_string()));
            }
        }
        cx.notify();
    }

    /// Resume host state without dialing or replacing retained runtime views.
    fn resume_link(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((mut connection, workspace)) = self.take_warm_link() else {
            return false;
        };
        let same_home = connection.home_matches(&self.session, self.home_key);
        let mut workspace = workspace;
        if !same_home && let Some(workspace) = workspace.as_mut() {
            workspace.release_all(&connection);
        }
        if !connection.activate_home(self.session.clone(), self.home_key, self.expected_authority) {
            if let Some(workspace) = workspace.as_mut() {
                workspace.release_all(&connection);
            }
            connection.close();
            return false;
        }
        let workspace = if let Some(workspace) = workspace.filter(|_| same_home) {
            self.layout_key = self.home_key;
            workspace
        } else {
            let mut workspace = Workspace::new(self.session.clone(), WorkspaceOptions::default());
            workspace.preserve_empty();
            workspace.restore(self.last_saved.clone());
            workspace
        };
        self.arm_wake(&connection, cx);
        self.link = Some(Link {
            connection,
            workspace,
        });
        self.connect_error = None;
        self.runtime_chrome = self.read_runtime_chrome();
        let epoch = self
            .link
            .as_ref()
            .map_or(0, |link| link.connection.connection_epoch());
        self.apply_activity(&[], Vec::new(), epoch, cx);
        cx.notify();
        true
    }

    pub(super) fn arm_wake(&mut self, connection: &Connection, cx: &mut Context<Self>) {
        let rx = connection.activity();
        self._wake = Some(cx.spawn(async move |this, cx| {
            loop {
                if rx.recv().await.is_err() {
                    break;
                }
                if this.update(cx, |desk, cx| desk.on_activity(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Snapshot before quitting releases views; no live handles or credentials are saved.
    pub(in crate::app) fn restart_request(&self) -> OpenRequest {
        OpenRequest {
            role: self.role,
            session: self.session.clone(),
            home_key: self.link_home_key().or(self.home_key),
            authority: self
                .link
                .as_ref()
                .and_then(|link| {
                    link.connection
                        .with_organization(|replica| replica.authority())
                })
                .or(self.expected_authority),
            remote: self.remote.clone(),
            layout_path: self.store.path.clone(),
            legacy_layout: self.store.legacy.clone(),
            stable_layout: self.stable_layout,
            resume_layout: self
                .link
                .as_ref()
                .filter(|link| link.workspace.restored())
                .and_then(|link| {
                    // A disconnected window still owns its last presentation.
                    // Keep its incarnation binding; normal restore revalidates it.
                    Some(persist::save_layout(
                        link.workspace.server_id()?,
                        &link.workspace.tabs,
                        &self.prefs,
                        Some(&link.workspace.active_id),
                    ))
                })
                .or_else(|| self.last_saved.clone()),
            ..OpenRequest::default()
        }
    }

    /// Save, then release every view and the connection. Runs once per window.
    pub fn prepare_close(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.fleet.clear();
        self.close_menu(cx);
        self.persist();
        self.flush_saves();
        self.release_link(false);
        cx.notify();
    }

    /// Release every view and close the connection. `replacing` also drops
    /// what belonged to it: agent alerts, the find bar, and pending uploads
    /// with a notice.
    pub(super) fn release_link(&mut self, replacing: bool) {
        self._wake = None;
        if let Some(link) = self.link.as_mut() {
            link.workspace.release_all(&link.connection);
            link.connection.close();
        }
        self.clear_projection(replacing);
    }

    /// Drop presentation and pending UI operations independently of the socket.
    pub(in crate::app) fn clear_projection(&mut self, replacing: bool) {
        self.capture.cancel_pending();
        let uploading = !self.uploads.is_empty() || self.uploads_reading > 0;
        self.uploads.clear();
        self.upload_epoch = self.upload_epoch.wrapping_add(1);
        if replacing && uploading {
            self.refuse_paste(
                "The terminal changed before the upload finished. Nothing was pasted.",
            );
        }
        self.panes.clear();
        if replacing {
            self.agent_seen.clear();
            self.commands.clear();
            self.feed.retain(|entry| entry.kind != Kind::Agent);
            self.forget_approvals();
            self.find = None;
            self.find_input = None;
        }
    }

    /// Resolve the existing home before redialing; never recreate a stale name.
    pub(super) fn prepare_reconnect_home(&mut self, cx: &mut Context<Self>) -> bool {
        if self.session_is_attached() {
            return true;
        }
        let key = self.link_home_key().or(self.home_key);
        let result = if let Some(remote) = self.remote.as_deref() {
            crate::connection::home::existing_remote_home(remote, &self.session, key)
        } else {
            host::start_server(&self.phux, &self.socket).and_then(|()| {
                if self.session.is_empty() {
                    crate::connection::home::open_home(&self.phux, &self.socket, None)
                } else {
                    crate::connection::home::existing_home(&self.socket, &self.session, key)
                }
            })
        };
        match result {
            // A launch that could not name a session reopens on the one
            // the server now offers, with that session's saved layout.
            Ok(name) if self.session.is_empty() => {
                let old = cx.entity_id();
                self.defer_desktop(cx, move |desktop, cx| desktop.rehome(old, name, cx));
                false
            }
            Ok(name) => {
                self.session = name.clone();
                self.workspace_call(|link| link.workspace.set_home_session(name));
                true
            }
            Err(error) => {
                self.post(Kind::Error, "Could not reconnect", Some(error));
                cx.notify();
                false
            }
        }
    }

    pub(crate) fn reconnect(&mut self, cx: &mut Context<Self>) {
        if !self.prepare_reconnect_home(cx) {
            return;
        }
        self.redial(cx);
    }

    /// Reattach after a managed handoff. The updater already performed the
    /// process work off-thread; this path never invokes a CLI subprocess.
    pub(super) fn redial(&mut self, cx: &mut Context<Self>) {
        // Managed updates bypass the CLI resolver, but must not lose the
        // identity already learned by the old connection when rebuilding it.
        self.home_key = self.link_home_key().or(self.home_key);
        self.persist();
        let snapshot = self.last_saved.clone();
        let closed = self
            .link
            .as_ref()
            .is_some_and(|link| matches!(link.connection.status, Status::Closed | Status::Failed));
        if closed || self.link.is_none() {
            self.release_link(true);
            self.attach(None, cx);
            if let Some(link) = self.link.as_mut() {
                link.workspace.restore(snapshot);
            }
        } else if let Some(link) = self.link.as_mut() {
            // The last frame stays. resync redials under it. Releasing the
            // views first is an empty window for the whole wait, and on a
            // slow link that wait is the reconnect.
            if let Err(error) = link.connection.reconnect() {
                self.post(Kind::Error, "Could not reconnect", Some(error.to_string()));
            }
        }
        cx.notify();
    }
}
