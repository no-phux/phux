//! Keep-awake: whether the server is holding off idle sleep, and the palette
//! rows that switch its mode (`defaults.keep-awake`).
//!
//! The server publishes its state at [`KeepAwake::STATUS_KEY`] while the mode
//! is not `off`; writing [`KeepAwake::MODE_KEY`] switches the mode until the
//! server restarts.

use phux_config::KeepAwake;
use phux_protocol::wire::frame::{FrameKind, Scope};

use super::{Desk, NoticeKind};
use crate::palette::{Item, Target};

/// The server's keep-awake record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Awake {
    /// `on`, `idle` or `unavailable`.
    pub state: String,
    pub working: u64,
    pub mode: KeepAwake,
}

impl Awake {
    /// The record at [`KeepAwake::STATUS_KEY`]; `None` when absent (the mode
    /// is `off`) or unreadable.
    pub fn parse(value: Option<&[u8]>) -> Option<Self> {
        let record: serde_json::Value = serde_json::from_slice(value?).ok()?;
        Some(Self {
            state: record.get("state")?.as_str()?.to_owned(),
            working: record
                .get("working")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            mode: serde_json::from_value(record.get("mode")?.clone()).ok()?,
        })
    }

    /// Whether the machine is being kept awake right now.
    pub fn holding(&self) -> bool {
        self.state == "on"
    }

    /// The indicator's words: what is keeping the machine awake.
    pub fn words(&self) -> String {
        match (self.mode, self.working) {
            (KeepAwake::Always, _) => "Keeping awake: always".to_owned(),
            (_, 1) => "Keeping awake: 1 agent working".to_owned(),
            (_, n) => format!("Keeping awake: {n} agents working"),
        }
    }
}

/// One palette row per mode, the one in force marked.
pub fn items(current: Option<&Awake>) -> Vec<Item> {
    let now = current.map_or(KeepAwake::Off, |awake| awake.mode);
    [
        (KeepAwake::Off, "Off", "let the machine sleep"),
        (
            KeepAwake::WhileAgentsWork,
            "While agents work",
            "stay awake while an agent works",
        ),
        (
            KeepAwake::Always,
            "Always",
            "stay awake while the server runs",
        ),
    ]
    .into_iter()
    .map(|(mode, name, detail)| Item {
        title: format!("Keep awake: {name}"),
        detail: if mode == now { "current" } else { detail }.to_owned(),
        kind: "keep-awake",
        target: Target::KeepAwake(mode),
    })
    .collect()
}

impl Desk {
    /// Switch the server's keep-awake mode until it restarts.
    pub(super) fn set_keep_awake(&mut self, mode: KeepAwake) {
        let Some(client) = self.connection.client.clone() else {
            self.say(NoticeKind::Error, "Not connected to a server");
            return;
        };
        client.queue_frame(&FrameKind::SetMetadata {
            request_id: client.next_request_id(),
            scope: Scope::Global,
            key: KeepAwake::MODE_KEY.to_owned(),
            value: serde_json::to_vec(&mode).unwrap_or_default(),
        });
        let name = serde_json::to_value(mode).ok();
        let name = name
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        self.say(NoticeKind::Info, format!("Keep awake: {name}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_reads_back_and_says_why() {
        let awake = Awake::parse(Some(
            br#"{"state":"on","working":2,"mode":"while-agents-work"}"#,
        ))
        .expect("record");
        assert!(awake.holding());
        assert_eq!(awake.words(), "Keeping awake: 2 agents working");
        let one = Awake {
            working: 1,
            ..awake.clone()
        };
        assert_eq!(one.words(), "Keeping awake: 1 agent working");
        let always = Awake {
            mode: KeepAwake::Always,
            ..awake
        };
        assert_eq!(always.words(), "Keeping awake: always");

        let idle = Awake::parse(Some(
            br#"{"state":"idle","working":0,"mode":"while-agents-work"}"#,
        ));
        assert!(!idle.expect("record").holding());
        assert_eq!(Awake::parse(None), None);
        // A server before the mode field: no record Rune can act on.
        assert_eq!(Awake::parse(Some(br#"{"state":"on","working":1}"#)), None);
    }

    #[test]
    fn the_palette_marks_the_mode_in_force() {
        let current = |awake: Option<&Awake>| {
            items(awake)
                .into_iter()
                .find(|item| item.detail == "current")
                .map(|item| item.target)
        };
        assert_eq!(current(None), Some(Target::KeepAwake(KeepAwake::Off)));
        let always = Awake {
            state: "on".to_owned(),
            working: 0,
            mode: KeepAwake::Always,
        };
        assert_eq!(
            current(Some(&always)),
            Some(Target::KeepAwake(KeepAwake::Always))
        );
    }
}
