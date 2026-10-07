//! The stable JSON error contract shared by every `--json` verb (ADR-0065
//! §4): a failure is one JSON line on stderr, stdout stays empty, and exit
//! codes are unchanged (`0` success, `1` miss / no server, `2` refusal / usage,
//! `3` partial view, `124`/`125` timeouts):
//!
//! ```json
//! {"schema_version":1,"error":{"code":"no_server","message":"..."},"remedy":"...","exit_code":1}
//! ```
//!
//! A `no_server` (or `transport`) failure after this process tried to
//! auto-start the server also carries the additive `error.auto_start_error`:
//! why that start failed, the server's own bind or config error included.
//!
//! Without `--json` the same failure prints prose (the no-server family keeps
//! its exact historical diagnostic). The closed `code` vocabulary lives in
//! [`codes`]; consumers read `docs/consumers/agents.md` §5.3.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use phux_client::attach::AttachError;

/// Version of the JSON error document. Additive fields do not bump it.
pub(crate) const ERROR_SCHEMA_VERSION: u8 = 1;

/// The closed vocabulary of stable error codes. Consumers branch on these,
/// so renaming one is a breaking change; add new codes here.
pub(crate) mod codes {
    /// No server is listening at the socket (connection refused / not found).
    pub(crate) const NO_SERVER: &str = "no_server";
    /// The server was there and closed the connection mid-command.
    pub(crate) const SERVER_DISCONNECTED: &str = "server_disconnected";
    /// Any other transport or protocol failure while talking to the server.
    pub(crate) const TRANSPORT: &str = "transport";
    /// The local coordinator did not become available inside the bounded
    /// `server --ensure` startup window.
    pub(crate) const SERVER_START_TIMEOUT: &str = "server_start_timeout";
    /// The caller interrupted or terminated `server --ensure` while startup
    /// coordination was in flight.
    pub(crate) const SERVER_START_CANCELLED: &str = "server_start_cancelled";
    /// The local coordinator could not be started for a non-timeout reason.
    pub(crate) const SERVER_START_FAILED: &str = "server_start_failed";
    /// A selector resolved against a complete view and matched nothing.
    pub(crate) const NO_SUCH_TARGET: &str = "no_such_target";
    /// A selector miss against an incomplete fleet view — the target may
    /// exist on an unreachable satellite (see `partial::EXIT_PARTIAL_VIEW`).
    pub(crate) const PARTIAL_VIEW: &str = "partial_view";
    /// A selector that does not parse under the target grammar.
    pub(crate) const INVALID_SELECTOR: &str = "invalid_selector";
    /// A session name the selector grammar could not address (empty, a
    /// leading `@`/`#`/`%`, or a `:` or `/@` inside).
    pub(crate) const INVALID_SESSION_NAME: &str = "invalid_session_name";
    /// `phux new` named a session that already exists.
    pub(crate) const SESSION_EXISTS: &str = "session_exists";
    /// The server did not confirm a session create (refused read-back, no
    /// registered result, or a missing capability for the requested shape).
    pub(crate) const SESSION_CREATE_FAILED: &str = "session_create_failed";
    /// The server refused a spawn (a missing command or cwd), or a placed
    /// spawn could not land at its target. Exit 1.
    pub(crate) const SPAWN_FAILED: &str = "spawn_failed";
    // The spatial edits' refusal codes live in `phux_client::spatial::codes`.
    /// A selector matched several panes where exactly one is required.
    pub(crate) const SELECTOR_NOT_SINGLE: &str = "selector_not_single";
    /// A spatial edit selector resolved to a satellite pane (local-only).
    pub(crate) const SATELLITE_TARGET: &str = "satellite_target";
    /// The server rejected a layout mutation for another reason.
    pub(crate) const LAYOUT_REJECTED: &str = "layout_rejected";
    /// The server predates cross-session moves (no `MOVE_RESOURCE` support).
    pub(crate) const SERVER_TOO_OLD: &str = "server_too_old";
    /// A local config-registry operation failed: a `[[plugins]]` /
    /// `[[remote]]` / `[[satellites]]` entry could not be read, validated,
    /// or written (phux-i0e8.8.3).
    pub(crate) const REGISTRY: &str = "registry";
    /// A `--remote` target could not become a dial: malformed, neither
    /// registered nor pairable, an unusable registry entry, or an `ssh://`
    /// entry, which carries an interactive attach only.
    pub(crate) const REMOTE_UNRESOLVED: &str = "remote_unresolved";
    /// A git workspace/worktree operation failed (not a repository, git
    /// itself failed, or its output did not parse).
    pub(crate) const WORKSPACE: &str = "workspace";
    /// A `phux workload` operation failed: the workload CA or registry could
    /// not be read, validated, or written, or enrollment material, a scope,
    /// an expiry, or a credential id was refused.
    pub(crate) const WORKLOAD: &str = "workload";
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
    /// `phux cockpit` ran on a host that is not macOS.
    pub(crate) const COCKPIT_UNSUPPORTED_PLATFORM: &str = "cockpit_unsupported_platform";
    /// No Phux Cockpit.app was found in the well-known locations.
    pub(crate) const COCKPIT_NOT_INSTALLED: &str = "cockpit_not_installed";
    /// `PHUX_COCKPIT_APP` pointed at something that is not an app bundle.
    pub(crate) const COCKPIT_INVALID_APP: &str = "cockpit_invalid_app";
    /// Launch Services could not open the app bundle.
    pub(crate) const COCKPIT_LAUNCH_FAILED: &str = "cockpit_launch_failed";
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
    /// PTY input definitely was not written (proven at a point other than
    /// lane contention — no PTY, a writer-side queue full or closed, or the
    /// pane's actor gone before handoff). Unlike `delivery_unknown`, retrying
    /// — under the same operation id or a fresh one — cannot type it twice.
    pub(crate) const INPUT_NOT_WRITTEN: &str = "input_not_written";
    /// The server-wide acknowledged input lane did not become available.
    pub(crate) const INPUT_BUSY: &str = "input_busy";
    /// The pane's occupant changed while acknowledged input was in flight.
    pub(crate) const UNKNOWN_OCCUPANT: &str = "unknown_occupant";
    /// An agent name is not addressable: it does not match the `%name`
    /// grammar, or it is a per-kind manifest constant (`claude` on every
    /// Claude pane) rather than a name someone chose (ADR-0075 point 4).
    pub(crate) const INVALID_AGENT_NAME: &str = "invalid_agent_name";
    /// A `%name` handed to an input verb resolved to a record with the
    /// withdrawn shape (a `kind` and `state: unknown`): its producer gave the
    /// claim up, so who is in the pane is unknown (ADR-0075 point 5). Exit 2;
    /// read-only verbs skip the gate.
    pub(crate) const AGENT_WITHDRAWN: &str = "agent_withdrawn";
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
    /// More than one enabled integration's `[agent_identity]` claims the
    /// requested agent kind, so the `--integration` default is ambiguous.
    pub(crate) const AMBIGUOUS_INTEGRATION: &str = "ambiguous_integration";
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
    /// The server did not advertise the feature a verb needs
    /// (`ServerFeature::ResourceKinds` for the agent session verbs), so the
    /// request was refused before any frame that server would drop. Exit 2.
    pub(crate) const UNSUPPORTED_SERVER: &str = "unsupported_server";
    /// The target Terminal has no live `AgentSession` child, so there is no
    /// session to log, emit into, or close. Exit 2.
    pub(crate) const NO_AGENT_SESSION: &str = "no_agent_session";
    /// The target Terminal has more than one live `AgentSession` child;
    /// address the session resource directly by `@N`. Exit 2.
    pub(crate) const AGENT_SESSION_AMBIGUOUS: &str = "agent_session_ambiguous";
    /// The resource is not of the kind the operation requires (a Terminal
    /// facet command sent to an `AgentSession`, or an append sent to a
    /// Terminal). Exit 2.
    pub(crate) const WRONG_RESOURCE_KIND: &str = "wrong_resource_kind";
    /// The caller is not the producer of a producer-fed resource's output
    /// stream. Exit 2.
    pub(crate) const NOT_PRODUCER: &str = "not_producer";
    /// An emitted record is not a valid `AgentEventsJsonlV1` record: an
    /// unknown `type`, a `data` that is not a JSON object, or a record over
    /// the per-record byte ceiling. Refused before any byte is sent when the
    /// client can tell. Exit 2.
    pub(crate) const RECORD_INVALID: &str = "record_invalid";
    /// An append exceeded the per-call ceiling or the resource's retained
    /// output ring. Exit 2.
    pub(crate) const OVERFLOW: &str = "overflow";
    /// `agent session open` named a parent Terminal the server does not hold.
    /// Exit 1.
    pub(crate) const PARENT_NOT_FOUND: &str = "parent_not_found";
    /// `agent session open` named a parent that is not a Terminal-kind
    /// resource (an `AgentSession` cannot parent another). Exit 2.
    pub(crate) const PARENT_KIND_MISMATCH: &str = "parent_kind_mismatch";
    /// The server refused to spawn the `AgentSession` for another reason.
    /// Exit 1.
    pub(crate) const AGENT_SESSION_REFUSED: &str = "agent_session_refused";
    /// An `--after` value is not a cursor (`SERVER_ID_HEX:SEQ`). Exit 2,
    /// before any connection.
    pub(crate) const INVALID_CURSOR: &str = "invalid_cursor";
    /// An `--idempotency-key` is not 32 hex digits, or is all zero. Exit 2,
    /// before any connection.
    pub(crate) const INVALID_IDEMPOTENCY_KEY: &str = "invalid_idempotency_key";
    /// The idempotency key was already used for a different request inside
    /// the server's horizon, so nothing was spawned. Exit 2.
    pub(crate) const IDEMPOTENCY_CONFLICT: &str = "idempotency_conflict";
    /// A result document could not be serialized as JSON.
    pub(crate) const JSON_SERIALIZE: &str = "json_serialize";
    /// A local state-directory write failed (a bug-report bundle).
    pub(crate) const IO: &str = "io";
    /// A client-side invariant this binary should never break.
    pub(crate) const INTERNAL_ERROR: &str = "internal_error";
    /// `snapshot --format html|vt` got an `Ok` reply with no rendered
    /// capture: either the server predates `--format` (its `GET_SCREEN`
    /// decoder silently drops the trailing byte, D9) or a D9-or-later
    /// server's render failed on its own engine. Exit 2.
    pub(crate) const FORMAT_UNSUPPORTED: &str = "format_unsupported";
    /// `phux watch --until` named a word outside the stream's `event`
    /// vocabulary.
    pub(crate) const UNKNOWN_EVENT_NAME: &str = "unknown_event_name";
    /// The server closed a `phux watch` stream before any `--until` event
    /// arrived.
    pub(crate) const STREAM_ENDED: &str = "stream_ended";
}

/// One CLI failure, carrying everything both output channels need: a stable
/// machine `code`, the human `message`, and the `remedy` naming the way out.
#[derive(Debug)]
pub(crate) struct CliError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) remedy: String,
    /// Why this process's auto-start of the server failed, when it tried;
    /// emitted as `error.auto_start_error`.
    pub(crate) auto_start_error: Option<String>,
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
            auto_start_error: None,
        }
    }
}

/// The last auto-start failure this process saw, and the socket it was for.
/// A CLI process makes one auto-start attempt and then dials, and the dial's
/// error is reported far from the attempt, so the failure is parked here.
static AUTO_START_FAILURE: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);

/// Remember that auto-starting a server on `socket_path` failed with
/// `error`, so a later [`no_server_error`] for that socket can say why.
pub(crate) fn record_auto_start_failure(socket_path: &Path, error: &std::io::Error) {
    if let Ok(mut slot) = AUTO_START_FAILURE.lock() {
        *slot = Some((socket_path.to_path_buf(), error.to_string()));
    }
}

/// The recorded auto-start failure for `socket_path`, if any.
fn auto_start_failure(socket_path: &Path) -> Option<String> {
    let slot = AUTO_START_FAILURE.lock().ok()?;
    slot.as_ref()
        .filter(|(socket, _)| socket == socket_path)
        .map(|(_, error)| error.clone())
}

/// The JSON error document for `err`. `exit_code` is embedded because a
/// consumer reading stderr may not see the process status.
pub(crate) fn error_document(err: &CliError, exit_code: u8) -> serde_json::Value {
    let mut error = serde_json::json!({ "code": err.code, "message": err.message });
    if let Some(auto_start_error) = &err.auto_start_error {
        error["auto_start_error"] = serde_json::Value::from(auto_start_error.as_str());
    }
    serde_json::json!({
        "schema_version": ERROR_SCHEMA_VERSION,
        "error": error,
        "remedy": err.remedy,
        "exit_code": exit_code,
    })
}

/// Report `err` on stderr (one JSON line under `json`, prose plus indented
/// remedy otherwise) and return the exit code. stdout is never touched.
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

/// Json-aware [`crate::commands::report_no_server`]: prose stays
/// byte-identical; under `json`, `no_server` for a connect failure or
/// `server_disconnected` / `transport` otherwise, always exit 1.
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

/// The [`CliError`] behind [`report_no_server`]'s JSON path, also embedded
/// by `phux status --json`.
pub(crate) fn no_server_error(err: &AttachError, socket_path: &Path, verb: &str) -> CliError {
    let mut cli_err = connect_error(err, socket_path, verb);
    if matches!(err, AttachError::Io(_)) {
        cli_err.auto_start_error = auto_start_failure(socket_path);
        if cli_err.auto_start_error.is_some() {
            cli_err.remedy = format!(
                "auto-starting a server failed: fix the cause in `error.auto_start_error`, \
                 then retry; {}",
                cli_err.remedy
            );
        }
    }
    cli_err
}

/// [`no_server_error`] before any auto-start context is added.
fn connect_error(err: &AttachError, socket_path: &Path, verb: &str) -> CliError {
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
        AttachError::Io(io_err) => CliError::new(
            codes::TRANSPORT,
            unreachable_socket_message(verb, io_err, socket_path),
            doctor,
        ),
        other => CliError::new(codes::TRANSPORT, format!("{verb} failed: {other}"), doctor),
    }
}

/// The sentence for a socket that exists in some form but cannot be dialed
/// (permission denied, not a socket, a path too long for `sockaddr_un`):
/// names the path and the OS reason, never the client's internal "attach
/// loop" wording.
pub(crate) fn unreachable_socket_message(
    verb: &str,
    io_err: &std::io::Error,
    socket_path: &Path,
) -> String {
    format!(
        "{verb}: cannot reach the server socket {}: {io_err}",
        socket_path.display()
    )
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

    /// A failed auto-start for the same socket rides along on the connect
    /// error as the additive `error.auto_start_error`; another socket's does
    /// not, and a disconnect is not a connect failure.
    #[test]
    fn a_recorded_auto_start_failure_rides_on_the_connect_error() {
        let socket = Path::new("/tmp/phux-auto-start-failure-test.sock");
        super::record_auto_start_failure(
            socket,
            &std::io::Error::other("server exited: failed to bind: Permission denied"),
        );
        let refused = AttachError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));

        let err = no_server_error(&refused, socket, "new");
        assert_eq!(err.code, codes::NO_SERVER);
        assert!(err.remedy.contains("auto_start_error"));
        let doc = error_document(&err, 1);
        assert_eq!(
            doc["error"]["auto_start_error"],
            "server exited: failed to bind: Permission denied"
        );

        let other = no_server_error(&refused, Path::new("/tmp/elsewhere.sock"), "new");
        assert!(other.auto_start_error.is_none());
        assert!(
            error_document(&other, 1)["error"]
                .get("auto_start_error")
                .is_none()
        );

        let gone = no_server_error(&AttachError::Disconnected, socket, "new");
        assert!(gone.auto_start_error.is_none());
    }

    /// The agent verbs' codes are part of the closed vocabulary frozen by
    /// ADR-0071 point 6.
    #[test]
    fn the_agent_verb_codes_live_in_the_single_closed_vocabulary() {
        assert_eq!(codes::NO_AGENT_RECORD, "no_agent_record");
        assert_eq!(codes::AGENT_DEPARTED, "agent_departed");
        assert_eq!(codes::AGENT_MISMATCH, "agent_mismatch");
        assert_eq!(codes::INVALID_KEY_SPEC, "invalid_key_spec");
        // Added after ADR-0071 point 6 was written.
        assert_eq!(codes::INPUT_NOT_WRITTEN, "input_not_written");
        assert_eq!(codes::DELIVERY_UNKNOWN, "delivery_unknown");
    }

    /// The agent session family (`agent session open|close`, `agent emit`,
    /// `agent log`) spells its codes here, `snake_case`, so the Claude shim and
    /// the MCP adapter branch on one vocabulary.
    #[test]
    fn the_agent_session_codes_live_in_the_single_closed_vocabulary() {
        assert_eq!(codes::UNSUPPORTED_SERVER, "unsupported_server");
        assert_eq!(codes::NO_AGENT_SESSION, "no_agent_session");
        assert_eq!(codes::AGENT_SESSION_AMBIGUOUS, "agent_session_ambiguous");
        assert_eq!(codes::WRONG_RESOURCE_KIND, "wrong_resource_kind");
        assert_eq!(codes::NOT_PRODUCER, "not_producer");
        assert_eq!(codes::RECORD_INVALID, "record_invalid");
        assert_eq!(codes::OVERFLOW, "overflow");
        assert_eq!(codes::PARENT_NOT_FOUND, "parent_not_found");
        assert_eq!(codes::PARENT_KIND_MISMATCH, "parent_kind_mismatch");
        assert_eq!(codes::AGENT_SESSION_REFUSED, "agent_session_refused");
        assert_eq!(codes::AGENT_WITHDRAWN, "agent_withdrawn");
    }
}
