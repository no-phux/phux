//! Input operations for the foreign client.

use super::*;

#[uniffi::export]
impl RemoteClient {
    /// Queue raw text locally; this never produces a delivery receipt.
    pub fn send_text(&self, terminal_id: String, text: String) -> WireInputSubmission {
        self.submit_raw(&terminal_id, |control, id| control.send_text(id, &text))
    }

    /// Queue one raw key locally; queueing is not server admission.
    pub fn send_key(
        &self,
        terminal_id: String,
        key: KeyPress,
        mods: KeyMods,
    ) -> WireInputSubmission {
        self.submit_raw(&terminal_id, |control, id| {
            control.send_key(id, keymap::key_event(&key, mods))
        })
    }

    /// Explicit untrusted raw paste; use `apply_paste` for a delivery receipt.
    pub fn send_paste(&self, terminal_id: String, text: String) -> WireInputSubmission {
        self.send_paste_with(terminal_id, text, PasteTrust::Untrusted)
    }

    /// Explicit trusted raw paste. User clipboard actions should instead use
    /// `apply_user_paste`, which preserves acknowledged input ordering.
    pub fn send_paste_trusted(&self, terminal_id: String, text: String) -> WireInputSubmission {
        self.send_paste_with(terminal_id, text, PasteTrust::Trusted)
    }

    /// Submit user clipboard input with the shared atomic delivery/order policy.
    /// `AwaitingReceipt` resolves through `take_input_deliveries`; `QueuedRaw` has
    /// no delivery receipt and must never be reported as acknowledged.
    pub fn apply_user_paste(&self, terminal_id: String, text: String) -> WireInputSubmission {
        let Some(id) = id::parse(&terminal_id) else {
            return WireInputSubmission::NotQueued;
        };
        let Some(client) = self.runtime_client() else {
            return WireInputSubmission::NotQueued;
        };
        client.apply_user_paste(&id, &text).into()
    }

    /// Submit a dedicated SDK intent to an exact resource and activation.
    /// Parse and validate before scheduling; `None` means nothing was sent.
    /// The outcome arrives as `NativeAgentResult`, never an input receipt.
    pub fn native_agent_command(
        &self,
        agent_session_id: String,
        activation: String,
        intent_json: String,
    ) -> Option<u64> {
        if intent_json.len() > phux_protocol::native_agent::MAX_NATIVE_PAYLOAD_BYTES {
            return None;
        }
        let intent: phux_protocol::native_agent::NativeIntent =
            serde_json::from_str(&intent_json).ok()?;
        intent.validate().ok()?;
        let id = id::parse(&agent_session_id)?;
        self.runtime_client()?
            .native_agent_command(&id, &activation, intent)
    }

    pub fn apply_line(&self, terminal_id: String, text: String) -> u64 {
        self.apply_acknowledged(&terminal_id, |client, id| client.apply_line(id, &text))
    }

    pub fn apply_paste(&self, terminal_id: String, text: String) -> u64 {
        self.apply_acknowledged(&terminal_id, |client, id| client.apply_paste(id, &text))
    }

    /// Answer the ask `question_id` names (the id an `AgentAsked` event
    /// carried) with `text` plus Enter, acknowledged. Refused, with the
    /// receipt saying why, when the pane is no longer asking that question
    /// or the text cannot be typed as one line; nothing is typed then.
    pub fn apply_answer(&self, terminal_id: String, question_id: String, text: String) -> u64 {
        self.apply_acknowledged(&terminal_id, |client, id| {
            client.apply_answer(id, &question_id, &text)
        })
    }

    pub fn apply_tab_completion(&self, terminal_id: String, text: String) -> u64 {
        self.apply_acknowledged(&terminal_id, |client, id| {
            client.apply_tab_completion(id, &text)
        })
    }

    pub fn put_file(&self, terminal_id: String, extension: String, data: Vec<u8>) -> u64 {
        let Some(client) = self.runtime_client() else {
            return 0;
        };
        let Some(id) = id::parse(&terminal_id) else {
            return client.refuse_file_upload("invalid terminal id");
        };
        client.put_file(id, extension, data)
    }

    pub fn transcribe(&self, transfer_id: u64) -> u32 {
        self.runtime_client()
            .map_or(0, |client| client.transcribe(transfer_id))
    }

    pub fn take_transcribe_receipts(&self) -> Vec<WireTranscribeReceipt> {
        self.runtime_client()
            .map(|client| {
                client
                    .take_transcribe_receipts()
                    .into_iter()
                    .map(Into::into)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn send_mouse(
        &self,
        terminal_id: String,
        action: MouseAction,
        button: MouseButton,
        mods: KeyMods,
        col: u32,
        row: u32,
    ) -> WireInputSubmission {
        self.submit_raw(&terminal_id, |control, id| {
            control.send_mouse(
                id,
                MouseEvent {
                    action: action.wire(),
                    button: button.wire(),
                    mods: mods.to_mod_set(),
                    x: f64::from(col),
                    y: f64::from(row),
                },
            )
        })
    }

    pub fn send_focus(&self, terminal_id: String, gained: bool) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.send_focus(
                id,
                if gained {
                    FocusEvent::Gained
                } else {
                    FocusEvent::Lost
                },
            )
        });
    }
}

impl RemoteClient {
    fn apply_acknowledged(
        &self,
        terminal_id: &str,
        apply: impl FnOnce(&Client, &ResourceId) -> u64,
    ) -> u64 {
        let Some(client) = self.runtime_client() else {
            return 0;
        };
        let Some(id) = id::parse(terminal_id) else {
            return client.refuse_acknowledged_input("invalid terminal id");
        };
        apply(&client, &id)
    }

    fn submit_raw(
        &self,
        terminal_id: &str,
        submit: impl FnOnce(&mut phux_client_runtime::control::ControlPlane, &ResourceId) -> bool,
    ) -> WireInputSubmission {
        self.with_terminal(terminal_id, |client, id| {
            client.with_control(|control| {
                if control.status() == phux_client_runtime::control::Status::Attached
                    && submit(control, id)
                {
                    WireInputSubmission::QueuedRaw
                } else {
                    WireInputSubmission::NotQueued
                }
            })
        })
        .unwrap_or(WireInputSubmission::NotQueued)
    }

    fn send_paste_with(
        &self,
        terminal_id: String,
        text: String,
        trust: PasteTrust,
    ) -> WireInputSubmission {
        self.submit_raw(&terminal_id, |control, id| {
            control.send_paste(id, text.into_bytes(), trust)
        })
    }
}

#[cfg(test)]
mod tests;
