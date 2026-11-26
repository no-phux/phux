//! Browse sessions without attaching or sending any input. The one visible
//! preview is a read-only snapshot; choosing the card commits navigation.
use phux_client_runtime::control::SessionDescriptor;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};
use rune::{Drag, Key, Phase};

use super::super::Desk;
use super::navigation::Page;
use super::touch::Feedback;

#[derive(Clone, Debug)]
pub(crate) struct Switcher {
    sessions: Vec<u32>,
    index: usize,
    namespace: u64,
    epoch: u64,
    request: Option<u32>,
    pub(crate) preview: Option<String>,
    pub(crate) caption: String,
    pub(crate) offset: f32,
}

#[derive(serde::Deserialize)]
struct ScreenRows {
    lines: Vec<String>,
}

impl Desk {
    pub(crate) fn open_phone_switcher(&mut self) {
        if self.modal().is_some() {
            return;
        }
        let sessions: Vec<_> = self.ordered_sessions().into_iter().map(|s| s.id).collect();
        if sessions.is_empty() {
            self.open_phone_sessions();
            return;
        }
        let current = self.selected_session_name();
        let index = current
            .as_deref()
            .and_then(|name| {
                let selected = self.connection.topology.as_ref()?.session_named(name)?.id;
                sessions.iter().position(|id| *id == selected)
            })
            .unwrap_or(0);
        self.open_phone_navigation(Page::Switcher);
        let Some(phone) = &mut self.phone else { return };
        phone.switcher = Some(Switcher {
            sessions,
            index,
            namespace: self.connection.namespace(),
            epoch: self.connection.view_epoch,
            request: None,
            preview: None,
            caption: String::new(),
            offset: 0.0,
        });
        self.request_session_preview();
    }

    pub(crate) fn switcher_selection(&self) -> Option<(&SessionDescriptor, usize, usize)> {
        let page = self.phone.as_ref()?.switcher.as_ref()?;
        if page.namespace != self.connection.namespace() || page.epoch != self.connection.view_epoch
        {
            return None;
        }
        let id = page.sessions[page.index];
        let session = self.connection.topology.as_ref()?.session(id)?;
        Some((session, page.index, page.sessions.len()))
    }

    fn request_session_preview(&mut self) {
        let Some((session, ..)) = self.switcher_selection() else {
            return;
        };
        let resources = self.addressable();
        let mut terminals = resources
            .iter()
            .filter(|r| r.session_id == session.id && r.kind.is_terminal());
        let pane = self
            .connection
            .shown
            .as_ref()
            .and_then(|shown| terminals.clone().find(|r| r.id == shown.resource))
            .or_else(|| terminals.next());
        let request = pane.and_then(|pane| {
            let client = self.connection.client.as_ref()?;
            let epoch = self.connection.view_epoch;
            let server = self.connection.view_server.as_deref();
            client.with_control(|control| {
                if control.connection_epoch() != epoch
                    || control.server().map(|info| info.id.as_slice()) != server
                {
                    return None;
                }
                Some(control.send_command(Command::GetScreen {
                    terminal_id: pane.id.clone(),
                    request_scrollback: None,
                    cells: false,
                    format: 0,
                }))
            })
        });
        if let Some(page) = self.phone.as_mut().and_then(|p| p.switcher.as_mut()) {
            page.request = request;
            page.preview = None;
            page.caption =
                pane.map_or_else(|| "No terminal panes".into(), |pane| pane.label.clone());
        }
    }

    pub(in crate::desk) fn session_preview_result(
        &mut self,
        id: u32,
        result: &CommandResult,
    ) -> bool {
        if self.switcher_selection().is_none() {
            return false;
        }
        let Some(page) = self.phone.as_mut().and_then(|p| p.switcher.as_mut()) else {
            return false;
        };
        if page.request != Some(id) {
            return false;
        }
        page.request = None;
        page.preview = match result {
            CommandResult::OkWith(CommandValue::Json(json)) => {
                serde_json::from_str::<ScreenRows>(json).ok().map(|screen| {
                    // Ignore empty rows under the prompt, preserve indentation and
                    // the row breaks Ghostty published, and keep recent content.
                    let end = screen
                        .lines
                        .iter()
                        .rposition(|line| !line.trim().is_empty())
                        .map_or(0, |at| at + 1);
                    screen.lines[end.saturating_sub(24)..end].join("\n")
                })
            }
            _ => None,
        };
        true
    }

    pub(crate) fn switcher_loading(&self) -> bool {
        self.phone
            .as_ref()
            .and_then(|p| p.switcher.as_ref())
            .is_some_and(|p| p.request.is_some())
    }

    pub(crate) fn step_phone_switcher(&mut self, step: isize) {
        let Some((_, index, len)) = self.switcher_selection() else {
            return;
        };
        let Some(next) = index.checked_add_signed(step).filter(|next| *next < len) else {
            return;
        };
        if let Some(phone) = &mut self.phone
            && let Some(page) = &mut phone.switcher
        {
            page.index = next;
            phone.feedback = Some(Feedback::Detent);
            self.request_session_preview();
        }
    }

    pub(crate) fn confirm_phone_switcher(&mut self) {
        if let Some((session, ..)) = self.switcher_selection() {
            let name = session.name.clone();
            self.close_phone_navigation();
            if self.selected_session_name().as_deref() != Some(name.as_str()) {
                self.show_session(&name);
            }
        }
    }

    pub(crate) fn switcher_drag(&mut self, drag: &Drag) {
        let Some(page) = self.phone.as_mut().and_then(|p| p.switcher.as_mut()) else {
            return;
        };
        let horizontal = drag.dx.abs() > drag.dy.abs();
        if drag.phase == Phase::Move {
            page.offset = if horizontal { drag.dx * 0.24 } else { 0.0 };
        } else {
            page.offset = 0.0;
        }
        if drag.phase == Phase::Up && !drag.cancelled() && horizontal && drag.dx.abs() >= 44.0 {
            self.step_phone_switcher(if drag.dx < 0.0 { 1 } else { -1 });
        }
    }

    pub(super) fn switcher_key(&mut self, key: &Key) -> bool {
        match key {
            Key::Left => self.step_phone_switcher(-1),
            Key::Right => self.step_phone_switcher(1),
            Key::Enter => self.confirm_phone_switcher(),
            _ => return false,
        }
        true
    }
}
