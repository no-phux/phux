//! Restoring and validating a native binding's carried operations.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "shares the parent's namespace"
)]
use super::*;

pub(super) fn restore_operations(binding: &mut NativeBinding) {
    binding.pending.clear();
    for operation in binding.operations.values_mut() {
        if matches!(
            operation.result.disposition,
            NativeDisposition::Completed | NativeDisposition::Refused
        ) {
            continue;
        }
        if operation.delivered {
            operation.result.disposition = NativeDisposition::Unknown;
            operation.result.detail = Some(
                "daemon upgraded after dispatch; host receipt required, no automatic replay".into(),
            );
        } else {
            operation.result.disposition = NativeDisposition::Refused;
            operation.result.detail =
                Some("daemon upgraded before dispatch; original connection authority ended".into());
            if let NativeIntent::Answer { question_id, .. } = &operation.command.intent {
                binding.answered.remove(question_id);
            }
        }
    }
}

pub(super) fn reconcile_publication(
    binding: &mut NativeBinding,
    blob: &crate::upgrade::blob::StateBlob,
) {
    if let Some(publication) = binding
        .publications
        .back_mut()
        .filter(|publication| publication.receipt.is_none())
    {
        let cut = blob
            .agent_sessions
            .iter()
            .find(|stream| ResourceId::local(stream.wire_id) == binding.agent);
        if let Some(accepted) = cut
            .into_iter()
            .flat_map(|stream| &stream.native_publications)
            .find(|accepted| {
                accepted.identity.id == publication.id
                    && accepted.identity.fingerprint == publication.fingerprint
            })
        {
            if let Some(view) = publication.view.as_ref() {
                if !binding.closed && publication.id > binding.publication_committed {
                    binding.view = view.clone();
                }
                // Actor acceptance is known, but a newly learned SDK
                // identity is not acknowledged until its restore recipe
                // was durably committed. The host retries this same id.
                if view.provider_session_id == binding.launch.native_id {
                    binding.publication_committed =
                        binding.publication_committed.max(publication.id);
                    publication.receipt = Some(NativePublicationReceipt {
                        publication_id: publication.id,
                        first_seq: accepted.accepted.first_seq,
                        last_seq: accepted.accepted.last_seq,
                    });
                    publication.view = None;
                }
            } else {
                binding.view.connected = false;
            }
        } else {
            // Actor did not accept before its sealed cut; safe to retry
            // the publication (never a model command) under the same id.
            binding.publications.pop_back();
            binding.publication_high_water = binding
                .publications
                .back()
                .map_or(0, |publication| publication.id);
        }
    }
}

pub(super) fn validate_completion(
    operation: &NativeOperation,
    result: &NativeCommandResult,
) -> Result<(), String> {
    if matches!(
        operation.result.disposition,
        NativeDisposition::Completed | NativeDisposition::Refused
    ) {
        return Err("operation already has a final receipt".into());
    }
    if result.disposition == NativeDisposition::Queued
        && result.detail.as_deref().is_none_or(str::is_empty)
    {
        return Err("queued receipt must identify its factual stage".into());
    }
    Ok(())
}
pub(super) fn projected_turn(state: NativeState, turn_id: Option<&String>) -> Option<String> {
    if state == NativeState::Done {
        None
    } else {
        turn_id.cloned()
    }
}

pub(super) fn validate_transcript(entry: &serde_json::Value) -> Result<(), String> {
    if entry.get("schema").and_then(serde_json::Value::as_str) == Some("phux.transcript/v1") {
        Ok(())
    } else {
        Err("transcript entry must use phux.transcript/v1".into())
    }
}

pub(super) fn validate_question(question: &NativeQuestion) -> Result<(), String> {
    if question.id.is_empty() {
        return Err("native question must preserve a nonempty provider id".into());
    }
    if !matches!(
        question.kind.as_str(),
        "confirm" | "select" | "input" | "editor" | "ask" | "permission" | "form"
    ) {
        return Err("unsupported native question kind".into());
    }
    validate_payload(question)
}
