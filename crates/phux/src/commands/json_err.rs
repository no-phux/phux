//! The stable JSON error contract shared by every `--json` verb
//! (ADR-0065 §4, phux-i0e8.8.2).
//!
//! A `--json` consumer's stdout is the document, so failures may not leak
//! into it — but "read stderr as prose" is no contract for a machine either.
//! Every converted verb therefore reports a failure as **one line of JSON on
//! stderr**, leaving stdout empty, with the established exit codes unchanged
//! (`0` success, `1` miss / no server, `2` refusal / usage, `3` partial
//! view, `124`/`125` timeouts):
//!
//! ```json
//! {"schema_version":1,"error":{"code":"no_server","message":"..."},"remedy":"...","exit_code":1}
//! ```
//!
//! Without `--json` the same failure prints prose (message, then the remedy
//! indented) — or, for the no-server family, the exact multi-line diagnostic
//! [`crate::commands::report_no_server`] has always printed, so scripts that
//! grep for it keep working.
//!
//! The `code` vocabulary is closed and lives in [`codes`], exported from this
//! one place so every stream that needs it (CLI, MCP adapter, docs) reads the
//! same list. The shape is documented for consumers in
//! `docs/consumers/agents.md` §5.3.

use std::path::Path;
use std::process::ExitCode;

use phux_client::attach::AttachError;

/// Version of the JSON error document. Additive fields do not bump it.
pub(crate) const ERROR_SCHEMA_VERSION: u8 = 1;

/// The closed vocabulary of stable error codes.
///
/// Consumers branch on these strings, so they are contract: renaming one is
/// a breaking change to `docs/consumers/agents.md` §5.3. Add new codes here
/// (with a doc line) rather than inventing strings at call sites.
pub(crate) mod codes {
    /// No server is listening at the socket (connection refused / not found).
    pub(crate) const NO_SERVER: &str = "no_server";
    /// The server was there and closed the connection mid-command.
    pub(crate) const SERVER_DISCONNECTED: &str = "server_disconnected";
    /// Any other transport or protocol failure while talking to the server.
    pub(crate) const TRANSPORT: &str = "transport";
    /// A selector resolved against a complete view and matched nothing.
    pub(crate) const NO_SUCH_TARGET: &str = "no_such_target";
    /// A selector miss against an incomplete fleet view — the target may
    /// exist on an unreachable satellite (see `partial::EXIT_PARTIAL_VIEW`).
    pub(crate) const PARTIAL_VIEW: &str = "partial_view";
    /// A selector that does not parse under the target grammar.
    pub(crate) const INVALID_SELECTOR: &str = "invalid_selector";
    /// A split ratio outside the open interval (0, 1).
    pub(crate) const INVALID_RATIO: &str = "invalid_ratio";
    /// A selector matched no panes (spatial edits require exactly one).
    pub(crate) const SELECTOR_MISS: &str = "selector_miss";
    /// A selector matched several panes where exactly one is required.
    pub(crate) const SELECTOR_NOT_SINGLE: &str = "selector_not_single";
    /// A spatial edit selector resolved to a satellite pane (local-only).
    pub(crate) const SATELLITE_TARGET: &str = "satellite_target";
    /// Two pane selectors that must differ resolved to the same pane.
    pub(crate) const SAME_PANE: &str = "same_pane";
    /// The panes of one spatial operation span more than one session.
    pub(crate) const CROSS_SESSION: &str = "cross_session";
    /// A pane whose owning session cannot be determined from the snapshot.
    pub(crate) const UNKNOWN_TERMINAL_SESSION: &str = "unknown_terminal_session";
    /// The session has no persisted layout to edit.
    pub(crate) const LAYOUT_MISSING: &str = "layout_missing";
    /// A selected pane is not present in the session's persisted layout.
    pub(crate) const PANE_NOT_IN_LAYOUT: &str = "pane_not_in_layout";
    /// The pane being inserted is already present in the persisted layout.
    pub(crate) const PANE_ALREADY_IN_LAYOUT: &str = "pane_already_in_layout";
    /// The server rejected a layout mutation for another reason.
    pub(crate) const LAYOUT_REJECTED: &str = "layout_rejected";
    /// The server predates cross-session moves (no `MOVE_TERMINAL` support).
    pub(crate) const SERVER_TOO_OLD: &str = "server_too_old";
    /// The server refused a cross-session `MOVE_TERMINAL` request.
    pub(crate) const MOVE_REFUSED: &str = "move_refused";
    /// A cross-session move committed but the post-move state read failed.
    pub(crate) const POST_MOVE_STATE_FAILED: &str = "post_move_state_failed";
    /// The destination pane changed windows while the move was in flight.
    pub(crate) const DESTINATION_CHANGED: &str = "destination_changed";
    /// The destination layout write after a cross-session move failed.
    pub(crate) const DESTINATION_LAYOUT_FAILED: &str = "destination_layout_failed";
    /// The source layout cleanup after a cross-session move failed.
    pub(crate) const SOURCE_LAYOUT_FAILED: &str = "source_layout_failed";
    /// A local config-registry operation failed: a `[[plugins]]` /
    /// `[[remote]]` / `[[satellites]]` entry could not be read, validated,
    /// or written (phux-i0e8.8.3).
    pub(crate) const REGISTRY: &str = "registry";
    /// A git workspace/worktree operation failed (not a repository, git
    /// itself failed, or its output did not parse).
    pub(crate) const WORKSPACE: &str = "workspace";
    /// `config check` could not run at all: the file was unreadable or the
    /// TOML did not parse. Exit 2, mirroring the prose path's distinct
    /// "could not check" status.
    pub(crate) const INVALID_CONFIG: &str = "invalid_config";
    /// `phux update` was pointed at something that is not a `vX.Y.Z` release
    /// tag.
    pub(crate) const UPDATE_INVALID_TAG: &str = "update_invalid_tag";
    /// This OS/architecture has no published release artifact, so there is
    /// nothing for `phux update` to install.
    pub(crate) const UPDATE_UNSUPPORTED_PLATFORM: &str = "update_unsupported_platform";
    /// The release index or a release artifact could not be downloaded.
    pub(crate) const UPDATE_FETCH_FAILED: &str = "update_fetch_failed";
    /// The published `.sha256` sidecar was unreadable or malformed, or the
    /// download could not be hashed — distinct from a mismatch, where the two
    /// digests were both readable and disagreed.
    pub(crate) const UPDATE_CHECKSUM_INVALID: &str = "update_checksum_invalid";
    /// The published checksum and the downloaded archive disagree. Nothing
    /// was unpacked and nothing was installed.
    pub(crate) const UPDATE_CHECKSUM_MISMATCH: &str = "update_checksum_mismatch";
    /// The verified archive did not contain what a phux release tarball
    /// contains.
    pub(crate) const UPDATE_ARCHIVE_REJECTED: &str = "update_archive_rejected";
    /// Staging or the atomic replacement failed.
    pub(crate) const UPDATE_INSTALL_FAILED: &str = "update_install_failed";
    /// `phux update --rollback` found nothing saved to roll back to.
    pub(crate) const UPDATE_NO_BACKUP: &str = "update_no_backup";
    /// The install lives in a read-only store (Nix/NixOS). Never mutated.
    pub(crate) const UPDATE_IMMUTABLE_STORE: &str = "update_immutable_store";
    /// The install is owned by a package manager (Homebrew, Cargo). The
    /// native command is the remedy.
    pub(crate) const UPDATE_PACKAGE_MANAGED: &str = "update_package_managed";
    /// The install is in no recognized location, so it is refused rather
    /// than overwritten on a guess.
    pub(crate) const UPDATE_SOURCE_UNSUPPORTED: &str = "update_source_unsupported";
    /// `phux agent explain --file` could not read the capture at all
    /// (missing path, unreadable file, stdin closed).
    pub(crate) const CAPTURE_UNREADABLE: &str = "capture_unreadable";
    /// The capture was read but is not a screen: JSON that is not a
    /// `ScreenState`, or a file with no rows in it.
    pub(crate) const CAPTURE_INVALID: &str = "capture_invalid";
    /// `phux agent explain --file` was given a `--kind` no loaded detection
    /// manifest claims (or `--kind` was omitted, which offline is required).
    pub(crate) const UNKNOWN_AGENT_KIND: &str = "unknown_agent_kind";
    /// The pane declares no `phux.agent/v1` record, so there is no agent
    /// lifecycle to wait on and no occupant to verify. Exit 2.
    pub(crate) const NO_AGENT_RECORD: &str = "no_agent_record";
    /// The agent went away mid-wait: the record was deleted, or its state
    /// withdrew to `unknown`. Neither a completion nor a timeout. Exit 1.
    pub(crate) const AGENT_DEPARTED: &str = "agent_departed";
    /// The pane's declared occupant is not the agent the caller named, so a
    /// write was refused before any byte reached the PTY. Exit 2.
    pub(crate) const AGENT_MISMATCH: &str = "agent_mismatch";
    /// A key spec did not parse. The whole batch is refused up front, so a
    /// typo in the third key cannot leave the first two delivered. Exit 2.
    pub(crate) const INVALID_KEY_SPEC: &str = "invalid_key_spec";
    /// A wait was asked for on a target set nothing in this build can ever
    /// satisfy, so it is refused up front instead of timing out. Exit 2.
    pub(crate) const UNSATISFIABLE_WAIT: &str = "unsatisfiable_wait";
    /// A prompt or answer contained no text.
    pub(crate) const PROMPT_EMPTY: &str = "prompt_empty";
    /// Prompt text contained a raw newline, whose submission count cannot be
    /// known without observing the pane's private bracketed-paste mode.
    pub(crate) const PROMPT_MULTILINE: &str = "prompt_multiline";
    /// An acknowledged input batch exceeded a client or protocol bound.
    pub(crate) const INPUT_TOO_LARGE: &str = "input_too_large";
    /// Another client holds the pane's input lease.
    pub(crate) const INPUT_LEASE_HELD: &str = "input_lease_held";
    /// Canonical input would truncate the batch, so nothing was written.
    pub(crate) const CANONICAL_LIMIT_EXCEEDED: &str = "canonical_limit_exceeded";
    /// A paste failed the pane's untrusted-input policy.
    pub(crate) const UNSAFE_PASTE: &str = "unsafe_paste";
    /// The server rejected an input batch structurally.
    pub(crate) const INVALID_INPUT_BATCH: &str = "invalid_input_batch";
    /// The authenticated peer is not allowed to perform the operation.
    pub(crate) const PERMISSION_DENIED: &str = "permission_denied";
    /// PTY delivery is indeterminate; retrying under a new id risks a duplicate.
    pub(crate) const DELIVERY_UNKNOWN: &str = "delivery_unknown";
    /// The server-wide acknowledged input lane did not become available.
    pub(crate) const INPUT_BUSY: &str = "input_busy";
    /// The pane's occupant changed while acknowledged input was in flight.
    pub(crate) const UNKNOWN_OCCUPANT: &str = "unknown_occupant";
    /// An addressable agent name does not match the `%name` grammar.
    pub(crate) const INVALID_AGENT_NAME: &str = "invalid_agent_name";
    /// The requested agent kind has no loaded detection manifest.
    pub(crate) const UNSUPPORTED_AGENT_KIND: &str = "unsupported_agent_kind";
    /// No detection manifests are available, so readiness is unenforceable.
    pub(crate) const AGENT_DETECTION_UNAVAILABLE: &str = "agent_detection_unavailable";
    /// Another pane already carries the requested explicit agent name.
    pub(crate) const AGENT_NAME_TAKEN: &str = "agent_name_taken";
    /// The target pane already hosts an identified agent.
    pub(crate) const AGENT_PANE_BUSY: &str = "agent_pane_busy";
    /// The target pane is not an available shell, or phux cannot prove it is.
    pub(crate) const AGENT_PANE_NOT_AVAILABLE: &str = "agent_pane_not_available";
    /// The integration working directory differs from the pane's directory.
    pub(crate) const AGENT_CWD_MISMATCH: &str = "agent_cwd_mismatch";
    /// A launch argv element cannot be encoded as one shell word.
    pub(crate) const INVALID_LAUNCH_ARGV: &str = "invalid_launch_argv";
    /// No enabled integration resolves the requested agent launch.
    pub(crate) const UNKNOWN_INTEGRATION: &str = "unknown_integration";
    /// Agent startup failed before a readiness assertion could be made.
    pub(crate) const AGENT_START_FAILED: &str = "agent_start_failed";
    /// The detector published a different kind from the one requested.
    pub(crate) const AGENT_KIND_MISMATCH: &str = "agent_kind_mismatch";
    /// Startup input may have landed, but delivery cannot be established.
    pub(crate) const AGENT_START_UNKNOWN: &str = "agent_start_unknown";
    /// Agent readiness did not arrive before the startup deadline.
    pub(crate) const AGENT_START_TIMEOUT: &str = "agent_start_timeout";
    /// The pane carries no current identified ask to answer.
    pub(crate) const NO_ACTIVE_ASK: &str = "no_active_ask";
    /// The live ask id differs from the one the caller observed.
    pub(crate) const ASK_STALE: &str = "ask_stale";
    /// The live ask has no id and cannot be correlated safely.
    pub(crate) const ASK_UNIDENTIFIED: &str = "ask_unidentified";
    /// A numbered choice was requested for an ask with no suggestions.
    pub(crate) const NO_SUGGESTIONS: &str = "no_suggestions";
    /// A numbered choice lies outside the ask's suggestion list.
    pub(crate) const CHOICE_OUT_OF_RANGE: &str = "choice_out_of_range";
    /// Free-form answer text is outside a closed suggestion set.
    pub(crate) const UNLISTED_ANSWER: &str = "unlisted_answer";
    /// Answer text is empty, multiline, or too large to deliver safely.
    pub(crate) const INVALID_ANSWER: &str = "invalid_answer";
    /// Neither a numbered choice nor answer text was supplied.
    pub(crate) const NO_ANSWER: &str = "no_answer";
    /// Answer delivery is indeterminate after handoff.
    pub(crate) const ANSWER_DELIVERY_UNKNOWN: &str = "answer_delivery_unknown";
    /// The server refused an answer batch for another typed reason.
    pub(crate) const ANSWER_REFUSED: &str = "answer_refused";
    /// A result document could not be serialized as JSON.
    pub(crate) const JSON_SERIALIZE: &str = "json_serialize";
    /// A client-side invariant this binary should never break.
    pub(crate) const INTERNAL_ERROR: &str = "internal_error";
}

/// One CLI failure, carrying everything both output channels need: a stable
/// machine `code`, the human `message`, and the `remedy` naming the way out.
#[derive(Debug)]
pub(crate) struct CliError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) remedy: String,
}

impl CliError {
    pub(crate) fn new(
        code: &'static str,
        message: impl Into<String>,
        remedy: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            remedy: remedy.into(),
        }
    }
}

/// The JSON error document for `err`, as emitted (one line) on stderr.
///
/// Pure, so the shape is unit-testable without capturing stderr. `exit_code`
/// is embedded because a `--json` consumer reading a pipeline's stderr may
/// not see the process status; the number in the document and the number the
/// process exits with are always the same.
pub(crate) fn error_document(err: &CliError, exit_code: u8) -> serde_json::Value {
    serde_json::json!({
        "schema_version": ERROR_SCHEMA_VERSION,
        "error": { "code": err.code, "message": err.message },
        "remedy": err.remedy,
        "exit_code": exit_code,
    })
}

/// Report `err` on stderr — one JSON line under `json`, prose plus the
/// indented remedy otherwise — and return `ExitCode::from(exit_code)`.
///
/// stdout is never touched: under `--json` it stays the document (empty on
/// failure), per the pinned output-hygiene discipline.
pub(crate) fn emit(json: bool, err: &CliError, exit_code: u8) -> ExitCode {
    if json {
        match serde_json::to_string(&error_document(err, exit_code)) {
            Ok(line) => eprintln!("{line}"),
            // A Value of strings and numbers cannot fail to serialize; the
            // fallback keeps the failure visible rather than silent.
            Err(_) => eprintln!("phux: {}", err.message),
        }
    } else {
        eprintln!("phux: {}", err.message);
        if !err.remedy.is_empty() {
            for line in err.remedy.lines() {
                eprintln!("  {line}");
            }
        }
    }
    ExitCode::from(exit_code)
}

/// Json-aware sibling of [`crate::commands::report_no_server`].
///
/// Without `json` it defers to that function, so the multi-line prose
/// diagnostic (start commands, server log, doctor pointer) stays
/// byte-identical to what scripts already grep for. With `json` it emits the
/// contract line — code [`codes::NO_SERVER`] for a connect-time failure,
/// [`codes::SERVER_DISCONNECTED`] / [`codes::TRANSPORT`] otherwise — always
/// with exit code 1, matching the prose path.
pub(crate) fn report_no_server(
    json: bool,
    err: &AttachError,
    socket_path: &Path,
    verb: &str,
) -> ExitCode {
    if !json {
        return crate::commands::report_no_server(err, socket_path, verb);
    }
    emit(true, &no_server_error(err, socket_path, verb), 1)
}

/// The [`CliError`] behind [`report_no_server`]'s JSON path, pure for tests.
///
/// `pub(crate)` so `phux status --json` can embed the same code / message /
/// remedy vocabulary inside its `{"running": false, ...}` answer document
/// instead of inventing a parallel no-server shape.
pub(crate) fn no_server_error(err: &AttachError, socket_path: &Path, verb: &str) -> CliError {
    let server_log = phux_server::telemetry::server_log_path();
    let doctor = format!(
        "server log: {}; run `phux doctor` for a health check",
        server_log.display()
    );
    match err {
        AttachError::Io(io_err)
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
            ) =>
        {
            CliError::new(
                codes::NO_SERVER,
                format!("no server running at {}", socket_path.display()),
                format!(
                    "start one with `phux` (attaches, auto-starting a server) or `phux server`; {doctor}"
                ),
            )
        }
        AttachError::Disconnected => CliError::new(
            codes::SERVER_DISCONNECTED,
            format!("server closed the connection during {verb}"),
            doctor,
        ),
        other => CliError::new(codes::TRANSPORT, format!("{verb} failed: {other}"), doctor),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use phux_client::attach::AttachError;

    use super::{CliError, ERROR_SCHEMA_VERSION, codes, error_document, no_server_error};

    /// The emit shape, pinned: `schema_version` 1, nested error object with
    /// code and message, top-level `remedy` and `exit_code`.
    #[test]
    fn error_document_pins_the_contract_shape() {
        let err = CliError::new(codes::NO_SERVER, "no server running at /tmp/x", "start one");
        let doc = error_document(&err, 1);
        assert_eq!(doc["schema_version"], u64::from(ERROR_SCHEMA_VERSION));
        assert_eq!(doc["error"]["code"], "no_server");
        assert_eq!(doc["error"]["message"], "no server running at /tmp/x");
        assert_eq!(doc["remedy"], "start one");
        assert_eq!(doc["exit_code"], 1);
        // Exactly the four top-level keys; a consumer may deny-list nothing.
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(4));
        // The emitted form is one line.
        let line = serde_json::to_string(&doc).unwrap_or_default();
        assert!(!line.contains('\n'), "error line must be single-line");
    }

    /// The three attach-error arms map onto the closed vocabulary, each with
    /// a non-empty remedy.
    #[test]
    fn no_server_errors_use_the_closed_vocabulary() {
        let socket = Path::new("/tmp/phux-test.sock");
        let refused = AttachError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let err = no_server_error(&refused, socket, "ls");
        assert_eq!(err.code, codes::NO_SERVER);
        assert!(err.message.contains("/tmp/phux-test.sock"));
        assert!(err.remedy.contains("`phux server`"));
        assert!(err.remedy.contains("phux doctor"));

        let err = no_server_error(&AttachError::Disconnected, socket, "kill");
        assert_eq!(err.code, codes::SERVER_DISCONNECTED);
        assert!(err.message.contains("kill"));
        assert!(!err.remedy.is_empty());

        let err = no_server_error(
            &AttachError::Refused("policy said no".to_owned()),
            socket,
            "tag",
        );
        assert_eq!(err.code, codes::TRANSPORT);
        assert!(err.message.contains("policy said no"));
        assert!(!err.remedy.is_empty());
    }

    /// phux-w7z2.35 / ADR-0071 point 7(a): the agent verbs' codes are part of
    /// the one closed vocabulary, spelled exactly as ADR-0071 point 6 froze
    /// them. A consumer branches on these strings, so the spellings are
    /// pinned here rather than only at the call sites that emit them.
    #[test]
    fn the_agent_verb_codes_live_in_the_single_closed_vocabulary() {
        assert_eq!(codes::NO_AGENT_RECORD, "no_agent_record");
        assert_eq!(codes::AGENT_DEPARTED, "agent_departed");
        assert_eq!(codes::AGENT_MISMATCH, "agent_mismatch");
        assert_eq!(codes::INVALID_KEY_SPEC, "invalid_key_spec");
        // phux-w7z2.42, added after ADR-0071 point 6 was written: it needs
        // carving into that enumeration in the PR that ships it.
        assert_eq!(codes::UNSATISFIABLE_WAIT, "unsatisfiable_wait");
    }
}
