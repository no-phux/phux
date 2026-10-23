//! Stable CLI/MCP projection of typed prompt failures and recovery facts.
use super::{AttachError, ExitCode, PromptError, Refusal, codes, json_err};

/// Map the client refusal enum onto the CLI's one closed error-code table.
pub(crate) const fn refusal_code(refusal: &Refusal) -> &'static str {
    match refusal {
        Refusal::EmptyText => codes::PROMPT_EMPTY,
        Refusal::MultilineText { .. } => codes::PROMPT_MULTILINE,
        Refusal::TooLarge { .. } => codes::INPUT_TOO_LARGE,
        Refusal::SatelliteTarget { .. } => codes::SATELLITE_TARGET,
        Refusal::NoAcknowledgedInput => codes::SERVER_TOO_OLD,
        Refusal::NoAgentRecord => codes::NO_AGENT_RECORD,
        Refusal::AgentMismatch(_) => codes::AGENT_MISMATCH,
        Refusal::InputLeaseHeld(_) => codes::INPUT_LEASE_HELD,
        Refusal::CanonicalLimitExceeded(_) => codes::CANONICAL_LIMIT_EXCEEDED,
        Refusal::UnsafePaste(_) => codes::UNSAFE_PASTE,
        Refusal::InvalidBatch(_) => codes::INVALID_INPUT_BATCH,
        Refusal::PermissionDenied(_) => codes::PERMISSION_DENIED,
    }
}

/// Map a [`PromptError`] onto its one published reading (exit code and
/// remedy), kept side by side so the table stays auditable.
#[allow(
    clippy::too_many_lines,
    reason = "the exit-code and remedy table is the contract; splitting it \
              hides which readings sit next to each other"
)]
fn failure_details(
    label: &str,
    socket_path: &std::path::Path,
    err: &PromptError,
    fallback_operation: &str,
) -> json_err::Refusal {
    let mut recovery = err.recovery();
    if recovery.operation_id.is_none() {
        recovery.operation_id = Some(fallback_operation.to_owned());
    }
    let project = |error: &json_err::CliError, exit| json_err::Refusal {
        err: error.clone().with_recovery(recovery.clone()),
        exit,
    };
    match err {
        PromptError::Refused(refusal) => project(
            &json_err::CliError::new(
                refusal_code(refusal),
                format!("{label}: {refusal}; nothing was sent"),
                remedy_for(refusal),
            ),
            crate::exit_codes::EXIT_USAGE,
        ),
        PromptError::LaneBusy {
            attempts,
            budget_ms,
            operation_id,
            ..
        } => project(
            &json_err::CliError::new(
                codes::INPUT_BUSY,
                format!(
                    "{label}: the pane's acknowledged input stayed busy across \
                     {attempts} attempts over {budget_ms}ms (operation {operation_id}); \
                     nothing was written"
                ),
                "nothing was written on any attempt, so re-running this command is safe. \
                 phux admits one acknowledged write per pane at a time, so another \
                 prompt or acknowledged write to this same pane was still unresolved: \
                 serialize writes to one pane; prompts to different panes do not collide",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::NotFound(message) => project(
            &json_err::CliError::new(
                json_err::codes::NO_SUCH_TARGET,
                format!("{label} is gone: {message}"),
                "re-resolve the target with `phux agent list`",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::DeliveryUnknown {
            operation_id,
            message,
        } => project(
            &json_err::CliError::new(
                codes::DELIVERY_UNKNOWN,
                format!("{label}: delivery is unknown (operation {operation_id}): {message}"),
                "DO NOT RESEND. Some, all, or none of the bytes reached the pane, and a \
                 batch reported unknown can still complete a moment later — resending \
                 under a new operation id is the duplicate turn this verb exists to \
                 prevent, and resending under the same one replays this same answer. \
                 Read the pane instead: `phux agent explain` or `phux snapshot`",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::NotWritten {
            operation_id,
            message,
        } => project(
            &json_err::CliError::new(
                codes::INPUT_NOT_WRITTEN,
                format!("{label}: nothing was written (operation {operation_id}): {message}"),
                "nothing reached the pane — this was proven, not assumed — so re-running \
                 this command is safe. A fresh invocation mints a fresh operation id and \
                 cannot deliver a duplicate turn, because no turn was delivered the first \
                 time",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::OccupantChanged {
            detail,
            operation_id,
            delivery,
        } => project(
            &json_err::CliError::new(
                codes::UNKNOWN_OCCUPANT,
                format!(
                    "{label}: {detail} while operation {operation_id} was in flight, so a \
                     '{}' delivery went to an unknown occupant",
                    delivery.as_str()
                ),
                "the occupant is re-read immediately before the write, but the record is \
                 published asynchronously, so a pane can change hands inside that window. \
                 Read the pane before resending",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::Departed {
            from,
            reason,
            operation_id,
            ..
        } => project(
            &json_err::CliError::new(
                codes::AGENT_DEPARTED,
                format!(
                    "{label}: the prompt was delivered (operation {operation_id}) but the \
                     agent departed from '{}' while waiting: {}",
                    from.as_str(),
                    reason.as_str()
                ),
                "a departure is not a completion — the agent went away rather than \
                 settling; inspect the pane with `phux agent explain` or `phux snapshot`",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::Transport(err @ AttachError::Io(_)) => project(
            &json_err::no_server_error(err, socket_path, "agent prompt"),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::Transport(err) => project(
            &json_err::CliError::new(
                json_err::codes::TRANSPORT,
                format!("agent prompt failed: {err}"),
                "the operation was not acknowledged, so delivery is unknown; read the pane \
                 before resending. Run `phux doctor` for a health check",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
    }
}

pub(super) fn report_failure(
    json: bool,
    label: &str,
    socket_path: &std::path::Path,
    err: &PromptError,
    fallback_operation: &str,
) -> ExitCode {
    if !json && let PromptError::Transport(err @ AttachError::Io(_)) = err {
        return json_err::report_no_server(false, err, socket_path, "agent prompt");
    }
    failure_details(label, socket_path, err, fallback_operation).emit(json)
}

/// The way out of each refusal, in the caller's own vocabulary.
fn remedy_for(refusal: &Refusal) -> String {
    match refusal {
        Refusal::EmptyText => "pass the prompt text as the second argument".to_owned(),
        Refusal::MultilineText { .. } => "send a single-line prompt. For multi-line content, \
             write it to a file the agent can read and prompt with the path; \
             `phux paste` followed by `phux send-keys Enter` is the explicit \
             form if you accept the N-submission risk"
            .to_owned(),
        Refusal::TooLarge { wire: false, .. } => "shorten the prompt, or put the long part in a \
             file the agent can read and prompt with the path. phux will not split one \
             prompt across operations: a truncated prompt that submits is worse than a \
             refused one"
            .to_owned(),
        Refusal::TooLarge { wire: true, .. } => {
            "the batch exceeds a protocol cap and cannot be sent as one operation, and \
             phux will not split it — a partial payload the receiver treats as complete \
             is the failure the cap exists to prevent"
                .to_owned()
        }
        Refusal::SatelliteTarget { host } => format!(
            "prompt it directly instead of through the hub:\n  phux host add {host} \
             <endpoint>\n  phux agent prompt --host {host} <TARGET> \"...\"\nThe hub is \
             not refusing to relay out of caution: it cannot own the receipt, and \
             synthesizing one would be a lie"
        ),
        Refusal::NoAcknowledgedInput => "upgrade the server (`phux update`), then retry. This \
             verb will not fall back to fire-and-forget input: `phux send-keys` is the \
             fire-and-forget verb, and it says so"
            .to_owned(),
        Refusal::NoAgentRecord => "`phux send-keys` addresses the pane and checks no identity — \
             use it when that is what you mean. To make this pane addressable as an \
             agent, run `phux agent set` or `phux agent install-claude`"
            .to_owned(),
        Refusal::AgentMismatch(_) => "the occupant is re-read immediately before the write; \
             re-resolve the target, or use `phux send-keys` if you meant the pane rather \
             than the agent"
            .to_owned(),
        Refusal::InputLeaseHeld(_) => "another consumer holds this pane's input authority. Wait \
             for it to finish and re-run — a prompt is the least urgent input in the \
             system, and phux will not seize a lease a human or a supervisor took"
            .to_owned(),
        Refusal::CanonicalLimitExceeded(_) => "zero bytes were written. The pane's line \
             discipline is in canonical mode, which is strong evidence the occupant is a \
             line-reading program rather than an agent TUI — check what is actually \
             running there"
            .to_owned(),
        Refusal::UnsafePaste(_) => "this verb sends TRUSTED pastes, so this refusal should be \
             unreachable; please report it"
            .to_owned(),
        Refusal::InvalidBatch(_) => "nothing was written. If the operation id already names \
             different input, that is a phux bug — please report it"
            .to_owned(),
        Refusal::PermissionDenied(_) => {
            "nothing was written; this client is not permitted to write to that pane".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

    use super::*;

    #[test]
    fn post_submit_io_failure_keeps_original_identity_and_unknown_effects() {
        let error = PromptError::Transport(AttachError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "reply lost after submit",
        )));
        let failure = failure_details(
            "@7",
            std::path::Path::new("/tmp/not-an-authority.sock"),
            &error,
            "original-input-id",
        );
        let document = json_err::error_document(&failure.err, failure.exit);
        assert_eq!(
            document["error"]["recovery"]["operation_id"],
            "original-input-id"
        );
        assert_eq!(document["error"]["recovery"]["effect_certainty"], "unknown");
        assert_eq!(
            document["error"]["recovery"]["scope"]["kind"],
            "input_server_incarnation"
        );
        assert!(document["error"]["recovery"]["scope"].get("id").is_none());
    }

    /// Every refusal carries a non-empty remedy and a distinct stable code.
    #[test]
    fn every_refusal_has_an_actionable_remedy_and_a_stable_code() {
        let refusals = [
            Refusal::EmptyText,
            Refusal::MultilineText { newlines: 2 },
            Refusal::TooLarge {
                unit: "bytes",
                measured: 5000,
                limit: 4096,
                wire: false,
            },
            Refusal::TooLarge {
                unit: "events",
                measured: 300,
                limit: 256,
                wire: true,
            },
            Refusal::SatelliteTarget {
                host: "devbox".to_owned(),
            },
            Refusal::NoAcknowledgedInput,
            Refusal::NoAgentRecord,
            Refusal::AgentMismatch("'builder', not 'reviewer'".to_owned()),
            Refusal::InputLeaseHeld("held".to_owned()),
            Refusal::CanonicalLimitExceeded("canonical".to_owned()),
            Refusal::UnsafePaste("unsafe".to_owned()),
            Refusal::InvalidBatch("invalid".to_owned()),
            Refusal::PermissionDenied("denied".to_owned()),
        ];
        let mut codes = std::collections::BTreeSet::new();
        for refusal in &refusals {
            let remedy = remedy_for(refusal);
            assert!(!remedy.is_empty(), "{refusal:?} has no remedy");
            assert!(!refusal_code(refusal).is_empty());
            codes.insert(refusal_code(refusal));
        }
        // The satellite remedy names the direct-dial escape, not just "no".
        assert!(
            remedy_for(&Refusal::SatelliteTarget {
                host: "devbox".to_owned()
            })
            .contains("phux host add devbox"),
        );
        // And the two size refusals both refuse to split.
        for wire in [true, false] {
            let remedy = remedy_for(&Refusal::TooLarge {
                unit: "bytes",
                measured: 99_999,
                limit: 4096,
                wire,
            });
            assert!(remedy.contains("split"), "{remedy}");
        }
        assert!(codes.len() >= 12, "codes must not collapse: {codes:?}");
    }
}
