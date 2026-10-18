//! How server receipts resolve a conversation's pending prompt, native
//! commands and question status, even while another face is showing.

use super::*;

impl Desk {
    /// Receipts resolve even while the terminal face or another tab is showing.
    pub(crate) fn conversation_receipts(&mut self, events: &[Event]) -> bool {
        let current_client = self
            .client()
            .map(|client| std::sync::Arc::downgrade(&client));
        let mut changed = false;
        for event in events {
            if matches!(event, Event::StatusChanged(status) if *status != Status::Attached) {
                for face in self
                    .conversation_faces
                    .values_mut()
                    .filter(|face| face.binding.activation.is_some())
                {
                    if !face.native_commands.is_empty() {
                        let unknown = receipt_message(
                            NativeDisposition::Unknown,
                            Some("Connection changed before receipt."),
                        );
                        for question in face.native_commands.values().flatten() {
                            if let Some(status) = face.question_status.get_mut(question) {
                                status.clone_from(&unknown);
                            }
                        }
                        face.notice = Some(unknown);
                        changed = true;
                    }
                }
            }
            if let Event::NativeAgentResult {
                agent_session_id,
                operation_id,
                disposition,
                detail,
            } = event
            {
                for face in self.conversation_faces.values_mut() {
                    if &face.binding.session != agent_session_id
                        || current_client
                            .as_ref()
                            .is_none_or(|client| !client.ptr_eq(&face.binding.client))
                    {
                        continue;
                    }
                    let Some(question) = face.native_commands.get(operation_id) else {
                        continue;
                    };
                    changed = true;
                    let message = receipt_message(*disposition, detail.as_deref());
                    if question.is_none()
                        && !face
                            .pending
                            .as_ref()
                            .is_some_and(|(id, _)| id == operation_id)
                        && *disposition == NativeDisposition::Refused
                    {
                        face.interrupted_turn = None;
                    }
                    if let Some(question) = question {
                        // Do not optimistically close native UI: queued is not provider consumption.
                        if matches!(disposition, NativeDisposition::Refused) {
                            face.question_status.remove(question);
                        } else if let Some(status) = face.question_status.get_mut(question) {
                            status.clone_from(&message);
                        }
                    }
                    resolve_prompt_receipt(
                        &mut face.pending,
                        &mut face.delivered,
                        *operation_id,
                        *disposition,
                    );
                    if face.draft_changed_after_submit {
                        face.delivered = None;
                    }
                    if matches!(
                        disposition,
                        NativeDisposition::Admitted
                            | NativeDisposition::Completed
                            | NativeDisposition::Refused
                    ) {
                        face.native_commands.remove(operation_id);
                    }
                    face.notice = Some(message);
                }
                continue;
            }
            let Event::InputDelivery {
                delivery_id,
                outcome,
                message,
                ..
            } = event
            else {
                continue;
            };
            for face in self.conversation_faces.values_mut() {
                if face.binding.activation.is_some() {
                    continue;
                }
                if current_client
                    .as_ref()
                    .is_none_or(|client| !client.ptr_eq(&face.binding.client))
                {
                    continue;
                }
                // A terminal-face paste can fence this same runtime even when
                // it was not submitted by a native composer.
                changed |= *outcome == DeliveryOutcome::Unknown && face.visible;
                if face
                    .pending
                    .as_ref()
                    .is_none_or(|(id, _)| id != delivery_id)
                {
                    continue;
                }
                let Some((_, submitted)) = face.pending.take() else {
                    continue;
                };
                changed = true;
                match outcome {
                    DeliveryOutcome::Delivered => {
                        if !face.draft_changed_after_submit {
                            face.delivered = Some(submitted);
                        }
                        face.error = None;
                    }
                    DeliveryOutcome::Refused => {
                        face.error = Some(if message.is_empty() {
                            "Message was refused; draft preserved.".into()
                        } else {
                            message.clone()
                        });
                    }
                    DeliveryOutcome::Unknown => {
                        face.error = Some("Delivery is unknown. Read the terminal before resending; the draft is preserved and is never retried automatically.".into());
                    }
                }
            }
        }
        changed
    }
}

pub(super) fn receipt_message(disposition: NativeDisposition, detail: Option<&str>) -> String {
    let stage = match disposition {
        NativeDisposition::Admitted => "Admitted by provider",
        NativeDisposition::Queued => "Queued — not yet confirmed consumed by provider",
        NativeDisposition::Completed => "Completed",
        NativeDisposition::Refused => "Refused — nothing retried; draft preserved",
        NativeDisposition::Unknown => {
            "Outcome unknown — inspect the conversation before resending; never retried automatically"
        }
    };
    match detail.filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("{stage}: {detail}"),
        None => stage.to_owned(),
    }
}

pub(super) fn resolve_prompt_receipt(
    pending: &mut Option<(u64, String)>,
    delivered: &mut Option<String>,
    operation: u64,
    disposition: NativeDisposition,
) {
    if matches!(
        disposition,
        NativeDisposition::Queued | NativeDisposition::Unknown
    ) {
        return;
    }
    if pending.as_ref().is_some_and(|(id, _)| *id == operation)
        && let Some((_, text)) = pending.take()
        && matches!(
            disposition,
            NativeDisposition::Admitted | NativeDisposition::Completed
        )
    {
        *delivered = Some(text);
    }
}
