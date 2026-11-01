//! Construction, configuration, and shell-facing access to a terminal view.
use super::*;

impl Terminal {
    /// Bind `view` of `terminal` on `client`. The entity is the only identity;
    /// a replaced view or client is a new entity, not a prop update.
    ///
    /// This entity does not own the runtime view. When closing it on a shared
    /// client, the host must call `client.destroy_view(view)`; dropping the
    /// entity alone does not release the `ViewId`.
    pub fn new(
        client: Arc<Client>,
        view: ViewId,
        terminal: ResourceId,
        mut settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::perf::mark_started();
        crate::fonts::install(cx);
        settings.sanitize();
        crate::fonts::resolve(&mut settings.font, cx);
        let focus = cx.focus_handle();
        let (input, _input_notices) = input_host::bind_input(
            Arc::downgrade(&client),
            view,
            terminal.clone(),
            focus.clone(),
            settings.option_as_alt,
            window,
            cx,
        );
        let focus_subscriptions = focus::subscribe(&focus, window, cx);
        Self {
            element_id: SharedString::from(format!("phux-terminal-{}", view.get())),
            client,
            view,
            terminal,
            settings,
            focus,
            observation: Arc::new(Mutex::new(paint::Observation::default())),
            scene: paint::SceneCache::default(),
            surface: presentation::Surface::default(),
            laser: laser::Laser::default(),
            input,
            _input_notices,
            window_remote: None,
            rebind: Arc::new(AtomicBool::new(false)),
            was_focused: false,
            reported_focus: None,
            pitch_scale: None,
            _focus_subscriptions: focus_subscriptions,
        }
    }

    /// The registry host this window attaches to. Absent is the local server.
    pub fn set_window_remote(&mut self, remote: Option<String>, cx: &mut Context<Self>) {
        if self.window_remote == remote {
            return;
        }
        self.window_remote = remote;
        let remote = self.window_remote.clone();
        self.input
            .update(cx, |state, _| state.set_window_remote(remote));
    }

    pub(super) fn host(&self) -> crate::attach::TerminalHost {
        crate::attach::terminal_host(self.window_remote.as_deref(), &self.terminal)
    }

    /// Replace font, theme, visibility, input policy, size ownership and app
    /// chords. Out-of-range metrics are clamped. Identity does not change.
    pub fn configure(
        &mut self,
        mut settings: Settings,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::fonts::install(cx);
        settings.sanitize();
        crate::fonts::resolve(&mut settings.font, cx);
        if self.settings.option_as_alt != settings.option_as_alt {
            self.input.update(cx, |state, _| {
                state.set_option_as_alt(settings.option_as_alt);
            });
        }
        // A desk refresh passes the same settings on every output frame.
        // Notifying here would acquire and prepare the terminal again.
        if self.settings == settings {
            return;
        }
        // The next render paints with these metrics. The replica must already
        // be dividing image pixels by them, which `fit` only does after paint.
        if !same_cell_metrics(&self.settings, &settings) {
            self.pitch_scale = None;
        }
        self.settings = settings;
        cx.notify();
    }

    /// Run `action` once, now, on this bound entity. It is not stored, so a
    /// later render cannot repeat it, and a request is never replayed onto a
    /// surface that was not yet bound.
    pub fn request(
        &mut self,
        action: HostAction,
        cx: &mut Context<Self>,
    ) -> Option<crate::input::InputNotice> {
        actions::run(action, &self.input, cx)
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The cursor cell as last painted, in window coordinates, for a popup
    /// that opens at the cursor.
    pub fn cursor_bounds(&self, cx: &gpui::App) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.input.read(cx).cursor_bounds()
    }

    /// The last paint. Glyph positions are filled only with `terminal-fixtures`.
    pub fn painted(&self) -> paint::Observation {
        self.observation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}
