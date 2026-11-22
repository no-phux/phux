//! The chief of staff: a tap of the right Command key opens a composer, and
//! Return hands what it holds to one designated ACP agent as a typed
//! `NativeIntent::Prompt`. Never terminal input, never a screen read: an
//! agent that takes no prompts over ACP gets nothing, and the composer says
//! why. Dictation apps (Hex and the like) paste into the focused field, so a
//! hold of the key while one listens opens the composer as it lets go.

use phux_client_runtime::control::{Status, Topology};
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{NativeDisposition, NativeIntent, NativeView};
use rune::{Key, KeyEvent, Modifier, PhysicalKeyCode, TextEdit, Tx};

use super::Desk;
use super::questions::response;

/// The session the chief runs in when `[rune] chief` is unset.
pub const DEFAULT_SESSION: &str = "chief";

/// The composer and the right Command tap that opens it.
#[derive(Default)]
pub struct Chief {
    pub open: bool,
    pub draft: TextEdit,
    /// The prompt on its way, until its receipt settles it.
    pending: Option<u64>,
    /// What the last send's receipt said.
    pub status: Option<String>,
    /// Why nothing was sent.
    pub error: Option<String>,
    /// Right Command is down and nothing else has happened since.
    armed: bool,
}

impl Chief {
    /// A prompt is on its way; the field holds until its receipt.
    pub const fn sending(&self) -> bool {
        self.pending.is_some()
    }

    /// Any key, click or focus change but the tap's own release.
    pub const fn disarm(&mut self) {
        self.armed = false;
    }

    /// The one receipt a command gets (an ACP prompt's is Queued: its turn's
    /// end reaches the transcript, not this window). Delivered clears the
    /// draft; Refused and Unknown keep it, and nothing is ever resent.
    pub fn receipt(
        &mut self,
        op: u64,
        disposition: NativeDisposition,
        detail: Option<&str>,
    ) -> bool {
        if self.pending != Some(op) {
            return false;
        }
        self.pending = None;
        let message = response::receipt(disposition, detail);
        match disposition {
            NativeDisposition::Admitted
            | NativeDisposition::Queued
            | NativeDisposition::Completed => {
                self.draft.clear();
                self.status = Some(message);
                self.error = None;
            }
            NativeDisposition::Refused | NativeDisposition::Unknown => {
                self.status = None;
                self.error = Some(message);
            }
        }
        true
    }

    /// The connection dropped under a prompt still on its way: it may or
    /// may not have arrived, so it is never resent.
    pub fn disconnected(&mut self) -> bool {
        if self.pending.take().is_none() {
            return false;
        }
        self.status = None;
        self.error = Some(response::receipt(
            NativeDisposition::Unknown,
            Some("Connection changed before receipt."),
        ));
        true
    }
}

/// The session `chief` names, or none when the tap is turned off.
pub fn session(chief: Option<&str>) -> Option<&str> {
    match chief.map(str::trim) {
        None | Some("") => Some(DEFAULT_SESSION),
        Some(name) if name.eq_ignore_ascii_case("none") => None,
        Some(name) => Some(name),
    }
}

/// The agent session in phux session `name` that takes typed prompts, and
/// its activation, re-read from `topology` and each session's `view`.
pub fn target(
    topology: Option<&Topology>,
    name: &str,
    view: impl Fn(&ResourceId) -> Option<NativeView>,
) -> Result<(ResourceId, String), String> {
    let topology = topology.ok_or("Not connected. Nothing was sent.")?;
    let terminals: Vec<&ResourceId> = (topology.panes.iter())
        .filter(|pane| pane.session_name == name)
        .map(|pane| &pane.terminal_id)
        .collect();
    if terminals.is_empty() {
        return Err(format!(
            "No session named ‘{name}’. Start one: phux new --json -s {name}, then phux launch opencode --acp --session {name}"
        ));
    }
    let agents = (topology.agent_sessions.iter()).filter(|agent| {
        agent
            .parent
            .as_ref()
            .is_some_and(|p| terminals.contains(&p))
    });
    let mut refusal = None;
    for agent in agents {
        let Some(view) = view(&agent.id) else {
            refusal
                .get_or_insert("The chief's ACP controller hasn't reported yet. Nothing was sent.");
            continue;
        };
        if !view.connected {
            refusal = Some(
                "The chief's ACP controller is disconnected. Nothing was sent; no terminal fallback.",
            );
        } else if !view.capabilities.prompt.is_supported() {
            refusal = Some("The chief's agent takes no prompts over ACP. Nothing was sent.");
        } else {
            return Ok((agent.id.clone(), view.activation));
        }
    }
    Err(refusal.map_or_else(
        || {
            format!(
                "‘{name}’ runs no ACP agent, and a terminal is never typed into. \
                 Start one: phux new --json -s {name}, then phux launch opencode --acp --session {name}"
            )
        },
        str::to_owned,
    ))
}

impl Desk {
    /// The phux session the chief runs in; none while the tap is off.
    pub fn chief_session(&self) -> Option<&str> {
        session(self.file.chief.as_deref())
    }

    /// Right Command pressed and released with nothing in between, while
    /// the terminal has the keyboard, opens the composer. The release must
    /// be the key's own: focus loss releases held keys with no physical key.
    pub(super) fn chief_tap(&mut self, event: &KeyEvent, tx: &Tx) -> bool {
        let Key::Modifier { modifier, pressed } = event.key else {
            self.chief.disarm();
            return false;
        };
        let right_command = modifier == Modifier::SuperRight;
        if pressed {
            // Alone: another modifier already down makes it a chord.
            let alone = !(event.mods.shift || event.mods.ctrl || event.mods.alt);
            self.chief.armed = right_command && alone && self.chief_can_open(tx);
            return false;
        }
        let tapped = right_command
            && std::mem::take(&mut self.chief.armed)
            && tx.physical_key() == Some(PhysicalKeyCode::SuperRight);
        if !right_command {
            self.chief.disarm();
        }
        if !(tapped && self.chief_can_open(tx)) {
            return false;
        }
        self.open_chief();
        true
    }

    /// Only over the terminal: a field or sheet with the keyboard may be
    /// taking chords Rune never sees.
    fn chief_can_open(&self, tx: &Tx) -> bool {
        self.phone.is_none()
            && self.focused
            && tx.focused().is_none()
            && self.modal().is_none()
            && self.chief_session().is_some()
    }

    /// The composer, focused, with whatever draft it kept.
    pub fn open_chief(&mut self) {
        self.chief.open = true;
        self.chief.error = None;
        if !self.chief.sending() {
            self.chief.status = None;
        }
        self.chief.draft.focused = true;
    }

    /// Put the composer away; an unsent draft stays for next time.
    pub fn close_chief(&mut self) {
        self.chief.open = false;
        self.chief.armed = false;
        self.chief.draft.focused = false;
    }

    /// Return: the draft to the chief as one typed prompt.
    pub fn send_chief(&mut self) {
        match self.schedule_chief() {
            Ok(op) => {
                self.chief.pending = Some(op);
                self.chief.error = None;
                self.chief.status = Some("Sending…".into());
            }
            Err(error) => {
                self.chief.error = Some(error);
            }
        }
    }

    fn schedule_chief(&self) -> Result<u64, String> {
        if self.chief.sending() {
            return Err("A prompt is already on its way.".into());
        }
        let text = self.chief.draft.text.trim();
        if text.is_empty() {
            return Err("Say or type something for the chief first.".into());
        }
        let name = self
            .chief_session()
            .ok_or("The chief is turned off in config.toml.")?;
        let client = (self.connection.client.as_ref()).ok_or("Not connected. Nothing was sent.")?;
        if client.status() != Status::Attached {
            return Err("Not connected. Nothing was sent.".into());
        }
        // Re-read the live topology and views: the cached ones may be behind.
        let engine = client.engine();
        let view = |id: &ResourceId| engine.as_ref().and_then(|e| e.native_agent_view(id));
        let (session, activation) = target(client.topology().as_ref(), name, view)?;
        let intent = NativeIntent::Prompt {
            text: text.to_owned(),
        };
        client
            .native_agent_command(&session, &activation, intent)
            .ok_or_else(|| "The prompt couldn't be scheduled. Nothing was sent.".to_owned())
    }
}

#[cfg(test)]
mod tests;
