//! Read access to the link, workspace, panes and runtime state, and the clock.

use super::*;

impl Desk {
    /// The home session's durable key, as the live connection learned it.
    pub(super) fn link_home_key(&self) -> Option<SessionKey> {
        self.link.as_ref()?.connection.home_key()
    }

    pub fn client(&self) -> Option<Arc<Client>> {
        Some(self.link.as_ref()?.connection.client.clone())
    }

    /// Change the focused view locally, then repaint it.
    pub(super) fn with_view(
        &self,
        cx: &mut Context<Self>,
        call: impl FnOnce(&Client, phux_client_runtime::ViewId),
    ) {
        let Some(view) = self.focused_view() else {
            return;
        };
        let Some(client) = self.client() else { return };
        call(&client, view);
        if let Some(id) = self
            .focused_placement()
            .map(|placement| placement.id.clone())
        {
            self.repaint_placement(&id, cx);
        }
    }

    /// Repaint one pane's cached terminal after a local view change, which
    /// republishes without a runtime wake.
    pub(super) fn repaint_placement(&self, placement_id: &str, cx: &mut Context<Self>) {
        if let Some(entity) = self.panes.get(placement_id) {
            entity.update(cx, |_, cx| cx.notify());
        }
    }

    pub(super) fn with_engine(
        &self,
        cx: &mut Context<Self>,
        call: impl FnOnce(&phux_client_runtime::engine::EngineHandle, phux_client_runtime::ViewId),
    ) {
        self.with_view(cx, |client, view| {
            if let Some(engine) = client.engine() {
                call(&engine, view);
            }
        });
    }

    pub(super) fn selection_text(&self) -> Option<String> {
        let view = self.focused_view()?;
        let client = self.client()?;
        let bytes = client.engine()?.view_selection_text(view).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The focused pane's view is scrolled back from live output.
    pub(crate) fn scrolled_back(&self) -> bool {
        self.session_is_attached()
            && self
                .focused_view()
                .and_then(|view| self.client().and_then(|client| client.acquire_view(view)))
                .is_some_and(|frame| !frame.scrollbar.at_tail())
    }

    /// The runtime state the chrome renders without an event of its own:
    /// terminal output and input receipts change it in paint-only batches.
    pub(super) fn read_runtime_chrome(&self) -> RuntimeChrome {
        let fenced = self.link.as_ref().map_or_else(Vec::new, |link| {
            self.placements()
                .filter_map(|placement| placement.terminal_id.as_ref())
                .filter(|id| link.connection.fenced(id))
                .cloned()
                .collect()
        });
        RuntimeChrome {
            scrolled_back: self.scrolled_back(),
            fenced,
        }
    }

    pub(crate) fn focused_view(&self) -> Option<phux_client_runtime::ViewId> {
        self.focused_placement()?.view_id
    }

    pub(super) fn focused_id(&self) -> Option<String> {
        Some(self.focused_placement()?.id.clone())
    }

    pub(crate) fn focused_terminal(&self) -> Option<ResourceId> {
        self.focused_placement()?.terminal_id.clone()
    }

    pub fn focused_placement(&self) -> Option<&Placement> {
        self.link.as_ref().and_then(|link| link.workspace.focused())
    }

    /// Every placement in every tab.
    pub(super) fn placements(&self) -> impl Iterator<Item = &Placement> {
        self.link.iter().flat_map(|link| {
            link.workspace
                .tabs
                .iter()
                .flat_map(|tab| crate::workspace::layout::placements(&tab.root))
        })
    }

    pub fn placement(&self, id: &str) -> Option<&Placement> {
        self.placements().find(|placement| placement.id == id)
    }

    pub(super) fn placement_of(&self, terminal: &ResourceId) -> Option<&Placement> {
        self.placements()
            .find(|placement| placement.terminal_id.as_ref() == Some(terminal))
    }

    pub(crate) fn active_tab(&self) -> Option<&crate::workspace::layout::DeskTab> {
        self.link.as_ref()?.workspace.active_tab()
    }

    pub(crate) fn active_tab_id(&self) -> Option<String> {
        self.active_tab().map(|tab| tab.id.clone())
    }

    /// The terminal's label by [`pane_label`].
    pub fn pane_title(&self, terminal: Option<&ResourceId>) -> String {
        let Some((link, terminal)) = self.link.as_ref().zip(terminal) else {
            return pane_label(None, None);
        };
        pane_label(
            link.connection.pane(terminal),
            link.connection.group_name(terminal),
        )
    }

    /// Human connection status and its tone, for the status bar and Settings.
    pub(crate) fn connection_label(&self) -> (String, Tone) {
        let Some(link) = self.link.as_ref() else {
            return ("Not connected".into(), Tone::Bad);
        };
        match link.connection.status {
            Status::Attached => ("Connected".into(), Tone::Good),
            Status::Idle | Status::Connecting | Status::Negotiated => {
                ("Connecting…".into(), Tone::Busy)
            }
            Status::Failed => ("Not connected".into(), Tone::Bad),
            Status::Closed => ("Disconnected".into(), Tone::Bad),
        }
    }

    /// The session is attached, so a terminal reacquire can paint.
    pub(crate) fn session_is_attached(&self) -> bool {
        self.link
            .as_ref()
            .is_some_and(|link| link.connection.status == Status::Attached)
    }

    /// Whether the connection needs the user to reconnect.
    pub(crate) fn unhealthy(&self) -> bool {
        self.link
            .as_ref()
            .is_none_or(|link| link.connection.status.is_terminal())
    }

    /// Open a modal from a pointer action: its controls take the keyboard.
    pub(crate) fn open_modal(&mut self, modal: Modal, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal != Modal::None {
            self.close_modal(window, cx);
        }
        self.modal = modal;
        self.after_command(window, cx);
    }

    pub(crate) fn toggle_session_collapsed(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(name) {
            self.collapsed.insert(name.to_owned());
        }
        cx.notify();
    }

    pub(super) fn pane_cwd(&self, terminal: Option<&ResourceId>) -> Option<String> {
        self.link.as_ref()?.connection.pane(terminal?)?.cwd.clone()
    }

    pub(crate) fn focused_pane(&self) -> Option<&phux_client_runtime::control::PaneDescriptor> {
        let terminal = self.focused_terminal()?;
        self.link.as_ref()?.connection.pane(&terminal)
    }

    pub fn agent(&self, terminal: &ResourceId) -> Option<&AgentInfo> {
        self.link.as_ref()?.connection.agents.get(terminal)
    }

    pub fn post(&mut self, kind: Kind, title: &str, body: Option<String>) {
        self.feed.post(kind, title, body);
    }

    /// Reveal a notification's terminal, if it has one, and mark it read.
    pub fn open_notification(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = self.feed.get(id).and_then(|entry| entry.terminal.clone());
        let approval = self
            .feed
            .get(id)
            .is_some_and(|entry| entry.kind == Kind::Approval);
        self.feed.read(id);
        // An approval is decided in the Inbox; opening one shows it there.
        if approval {
            if self.modal != Modal::Notifications {
                self.execute("notifications", window, cx);
            }
            return;
        }
        if self.modal == Modal::Notifications {
            self.close_modal(window, cx);
        }
        if let Some(terminal) = terminal {
            self.reveal_terminal(&terminal);
        }
        self.after_command(window, cx);
    }

    /// Mirror new agent, approval, and error entries to the system
    /// notification center while no Phux window is active. A click comes
    /// back through [`super::super::session::Desktop`] by its [`SystemTag`]; an
    /// approval's tag is shared by every window that holds it.
    pub(crate) fn flush_system(&mut self, cx: &mut Context<Self>) {
        let outbound = self.feed.take_outbound();
        if outbound.is_empty() || cx.active_window().is_some() {
            return;
        }
        let desk = cx.entity_id().as_u64();
        for entry in outbound {
            cx.show_system_notification(gpui::SystemNotification {
                tag: SystemTag::of(desk, &entry).to_string().into(),
                title: entry.title.into(),
                body: entry.body.unwrap_or_default().into(),
                actions: Vec::new(),
            });
        }
    }

    /// Expire the peek card and keep agent ages current. Armed from every
    /// render; it wakes when the next label changes and ends when nothing
    /// needs it.
    pub(crate) fn ensure_clock(&mut self, cx: &mut Context<Self>) {
        let Some(wait) = self.clock_wait() else {
            return;
        };
        let at = Instant::now() + wait;
        if self._clock.is_some() && self.clock_at.is_some_and(|armed| armed <= at) {
            return;
        }
        self.clock_at = Some(at);
        self._clock = Some(cx.spawn(async move |this, cx| {
            smol::Timer::after(wait).await;
            let _ = this.update(cx, |desk, cx| {
                desk._clock = None;
                desk.feed.expire_peek(Instant::now());
                desk.ensure_clock(cx);
                cx.notify();
            });
        }));
    }

    /// How long until the peek card expires or an agent's age label changes.
    pub(super) fn clock_wait(&self) -> Option<Duration> {
        let peek = self
            .feed
            .peek_expires()
            .map(|at| at.saturating_duration_since(Instant::now()));
        let now = crate::connection::now_ms();
        let ages = self.link.iter().flat_map(|link| {
            link.connection
                .agents
                .values()
                .map(move |info| super::super::ui::ago_wait(info.changed_at_ms, now))
        });
        peek.into_iter()
            .chain(ages.min().map(Duration::from_millis))
            .min()
    }

    pub(super) fn retain_panes(&mut self) {
        let live: Vec<_> = self
            .placements()
            .map(|placement| placement.id.clone())
            .collect();
        self.panes.retain(&live);
        let connection = self.link.as_ref().map(|link| &link.connection);
        self.conversation_faces.retain(|placement, face| {
            face.retain_for_placement(live.contains(placement), connection)
        });
    }

    pub fn panes(&self) -> &Panes {
        &self.panes
    }

    pub fn panes_mut(&mut self) -> &mut Panes {
        &mut self.panes
    }
}
