//! One-shot presentation handoff across an installed desktop update.

use super::*;
use anyhow::Context as _;
use phux_protocol::ids::AuthorityId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
struct Restart {
    version: u8,
    windows: Vec<SavedWindow>,
    active: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct SavedWindow {
    role: WindowRole,
    session: String,
    key: Option<SessionKey>,
    authority: Option<AuthorityId>,
    remote: Option<String>,
    path: PathBuf,
    legacy: Option<PathBuf>,
    stable: bool,
    layout: Option<Value>,
}

impl SavedWindow {
    fn from_request(request: OpenRequest) -> Self {
        Self {
            role: request.role,
            session: request.session,
            key: request.home_key,
            authority: request.authority,
            remote: request.remote,
            path: request.layout_path,
            legacy: request.legacy_layout,
            stable: request.stable_layout,
            layout: request.resume_layout.as_ref().map(persist::layout_value),
        }
    }

    fn request(self) -> anyhow::Result<OpenRequest> {
        anyhow::ensure!(!self.session.is_empty(), "Missing restart session");
        let resume_layout = self
            .layout
            .as_ref()
            .map(|value| persist::parse_layout(value).context("Invalid restart layout"))
            .transpose()?;
        Ok(OpenRequest {
            role: self.role,
            session: self.session,
            home_key: self.key,
            authority: self.authority,
            remote: self.remote,
            layout_path: self.path,
            legacy_layout: self.legacy,
            stable_layout: self.stable,
            resume_layout,
            ..OpenRequest::default()
        })
    }
}

impl Desktop {
    /// Keep a restart snapshot out of the user's state directory.
    #[cfg(test)]
    pub(in crate::app) fn set_socket_for_test(&mut self, socket: std::path::PathBuf) {
        self.socket = socket;
    }

    fn restart_store(&self) -> FileLayoutStore {
        FileLayoutStore::new(
            persist::layout_paths(&self.socket.display().to_string(), "")
                .layout
                .with_file_name("restart.json"),
            None,
        )
    }

    /// Called before quit can release terminal views, including fresh windows
    /// which deliberately do not write the session's ordinary layout file.
    pub(in crate::app) fn save_restart(&self, cx: &App) -> anyhow::Result<()> {
        let mut windows = Vec::new();
        let mut active = None;
        for opened in self.opened() {
            let request = opened.desk.read(cx).restart_request();
            if request.session.is_empty() {
                continue;
            }
            if cx
                .active_window()
                .is_some_and(|window| window.window_id() == opened.handle.window_id())
            {
                active = Some(windows.len());
            }
            windows.push(SavedWindow::from_request(request));
        }
        self.restart_store().write(&serde_json::to_value(Restart {
            version: 1,
            windows,
            active,
        })?)?;
        Ok(())
    }

    pub(super) fn restore_update(
        &mut self,
        focus: bool,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Option<WindowHandle<Root>>> {
        let store = self.restart_store();
        let persist::StoreValue::Present(value) = store.read() else {
            return Ok(None);
        };
        let requests = match restart_requests(value) {
            Ok(requests) => requests,
            Err(error) => {
                log::error!("Could not restore desktop restart: {error:#}");
                return Ok(None);
            }
        };
        let mut foreground = None;
        // The active window is opened last so native activation leaves it in front.
        for request in requests {
            let quick = request.role == WindowRole::Quick;
            let handle = self.open_window(request, focus, quick, cx)?;
            foreground = Some(handle);
        }
        if let Err(error) = std::fs::remove_file(&store.path) {
            log::error!("Could not consume desktop restart: {error}");
        }
        Ok(foreground)
    }
}

fn restart_requests(value: Value) -> anyhow::Result<Vec<OpenRequest>> {
    let mut restart: Restart = serde_json::from_value(value)?;
    anyhow::ensure!(restart.version == 1, "Unsupported desktop restart version");
    anyhow::ensure!(
        restart
            .windows
            .iter()
            .filter(|window| window.role == WindowRole::Quick)
            .count()
            <= 1,
        "Duplicate quick window"
    );
    if let Some(active) = restart.active {
        anyhow::ensure!(
            active < restart.windows.len(),
            "Invalid active restart window"
        );
        let window = restart.windows.remove(active);
        restart.windows.push(window);
    }
    restart
        .windows
        .into_iter()
        .map(SavedWindow::request)
        .collect()
}

#[cfg(test)]
mod tests;
