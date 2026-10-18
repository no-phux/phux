//! Recent projects: the persisted history, its errors, and opening one.

use super::*;

impl Desktop {
    pub(super) fn listen_history_errors(&mut self, cx: &mut Context<Self>) {
        let errors = self
            .recent_projects
            .as_ref()
            .expect("desktop launched")
            .errors();
        self._history_errors = Some(cx.spawn(async move |this, cx| {
            while let Ok(error) = errors.recv().await {
                let _ = this.update(cx, |desktop, cx| {
                    for opened in desktop.opened() {
                        opened.desk.update(cx, |desk, cx| {
                            desk.post(
                                super::super::notifications::Kind::Error,
                                "Could not save recent folders",
                                Some(error.clone()),
                            );
                            cx.notify();
                        });
                    }
                });
            }
        }));
    }

    pub(crate) fn record_recent_project(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> std::io::Result<()> {
        self.recent_projects
            .as_mut()
            .expect("desktop launched")
            .record(path)?;
        self.publish_recent_projects(cx);
        Ok(())
    }

    pub(crate) fn forget_recent_project(
        &mut self,
        path: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> std::io::Result<()> {
        self.recent_projects
            .as_mut()
            .expect("desktop launched")
            .forget(path)?;
        self.publish_recent_projects(cx);
        Ok(())
    }

    /// A window may render while this entity is leased by `open_window`;
    /// publish the small snapshot after the current GPUI update completes.
    pub(super) fn publish_recent_projects(&self, cx: &mut Context<Self>) {
        let paths = self.recent_projects().to_vec();
        let windows: Vec<_> = self
            .opened()
            .map(|opened| opened.desk.downgrade())
            .collect();
        cx.defer(move |cx| {
            for window in windows {
                let _ = window.update(cx, |desk, cx| {
                    desk.recent_projects = paths.clone();
                    cx.notify();
                });
            }
        });
    }

    /// A folder is a durable session, not a new terminal in whichever
    /// session happened to be focused. Reuse its open window; otherwise load
    /// the layout scoped to this socket and session.
    pub fn open_project(
        &mut self,
        session: String,
        terminal: ResourceId,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if let Some(opened) = self.windows.iter().find(|opened| {
            opened.session == session && opened.role == WindowRole::Main && opened.remote.is_none()
        }) {
            cx.activate(true);
            opened
                .handle
                .update(cx, |_, window, _| window.activate_window())?;
            opened
                .desk
                .update(cx, |desk, cx| desk.show_project_terminal(terminal, cx));
            return Ok(());
        }
        let paths = crate::workspace::layout_paths(&self.socket.display().to_string(), &session);
        let request = OpenRequest {
            session,
            initial_terminal: Some(terminal),
            layout_path: paths.layout,
            stable_layout: true,
            ..OpenRequest::default()
        };
        self.open_window(request, true, false, cx)?;
        Ok(())
    }
}
