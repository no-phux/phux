//! Sentences for server replies the client did not expect.
//!
//! Every consumer of the wire used to print unexpected replies with
//! `{other:?}` — a `Debug` dump of a wire enum, which tells a user nothing
//! and tells a script less (phux-i0e8.7.3). The two helpers here replace
//! those sites with the only two honest sentences available:
//!
//! * the server answered with a structured `Error` — its own diagnostic is
//!   the best sentence anyone has, so surface it verbatim;
//! * the server answered with a *kind* this client does not know — on a
//!   `#[non_exhaustive]` wire vocabulary that means the two binaries
//!   disagree about the protocol, so say so and name the remedy
//!   (`phux doctor`, which prints both sides' versions).
//!
//! Neither sentence contains a `Debug` rendering: the unknown kind is by
//! definition meaningless to the human reading it.

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};

/// Explain a [`CommandResult`] a caller did not expect, attributed to `verb`.
///
/// `Error` surfaces the server's own message verbatim (`"{verb} failed:
/// {message}"`); every other kind is a reply this client has no arm for,
/// which on this wire means version skew — it becomes
/// [`unexpected_reply`]'s doctor pointer.
#[must_use]
pub fn explain_unexpected(verb: &str, result: &CommandResult) -> String {
    match result {
        CommandResult::Error { message, .. } => format!("{verb} failed: {message}"),
        _ => unexpected_reply(verb),
    }
}

/// The sentence for a reply kind this client cannot interpret at all.
///
/// For the sites where the unexpected value is not a [`CommandResult`] —
/// a `SPAWN_TERMINAL` answer, a handshake frame — but the diagnosis is the
/// same: the server spoke a vocabulary this client does not have, so the
/// versions likely differ. Names `phux doctor` and this client's protocol
/// triple so the user can compare without guessing.
#[must_use]
pub fn unexpected_reply(verb: &str) -> String {
    format!(
        "unexpected {verb} reply; client and server versions may differ - \
         run `phux doctor` (client protocol {}.{}.{})",
        PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor, PROTOCOL_VERSION.patch,
    )
}

/// Human-readable rendering of a wire [`ErrorCode`]: the variant name as
/// lowercase spaced words (`TerminalNotFound` becomes `terminal not found`).
///
/// [`ErrorCode`] is `#[non_exhaustive]`, so a per-variant match here would
/// need a wildcard arm that silently falls back to `Debug` for every code a
/// newer protocol adds — exactly the render this module exists to remove.
/// Deriving the words from the variant name instead covers future codes for
/// free, and the digit/uppercase handling keeps a hypothetical `Http2Error`
/// readable (`http2 error`).
#[must_use]
pub fn error_code_label(code: ErrorCode) -> String {
    let name = format!("{code:?}");
    let mut label = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                label.push(' ');
            }
            label.push(ch.to_ascii_lowercase());
        } else {
            label.push(ch);
        }
    }
    label
}

#[cfg(test)]
mod tests {
    use phux_protocol::wire::frame::CommandValue;

    use super::*;

    /// The server's own sentence is the diagnostic; it must pass through
    /// verbatim, prefixed only by the verb attribution.
    #[test]
    fn error_message_passes_through_verbatim() {
        let result = CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "pane @9 does not exist".to_owned(),
        };
        assert_eq!(
            explain_unexpected("kill", &result),
            "kill failed: pane @9 does not exist",
        );
    }

    /// A reply kind with no arm means version skew: the sentence must name
    /// the remedy (`phux doctor`) and this client's protocol triple.
    #[test]
    fn unknown_kind_names_doctor_and_the_protocol_triple() {
        let result = CommandResult::OkWith(CommandValue::Json("{}".to_owned()));
        let sentence = explain_unexpected("GET_STATE", &result);
        assert!(
            sentence.contains("unexpected GET_STATE reply"),
            "{sentence}"
        );
        assert!(
            sentence.contains("client and server versions may differ"),
            "{sentence}"
        );
        assert!(sentence.contains("run `phux doctor`"), "{sentence}");
        let triple = format!(
            "client protocol {}.{}.{}",
            PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor, PROTOCOL_VERSION.patch,
        );
        assert!(sentence.contains(&triple), "{sentence}");
    }

    /// The whole point of the pass: no `Debug` rendering of wire enums
    /// reaches a user. A brace in the sentence is the old bug back.
    #[test]
    fn no_debug_braces_in_any_sentence() {
        let unexpected = CommandResult::OkWith(CommandValue::Bytes(vec![1, 2, 3]));
        for sentence in [
            explain_unexpected("ROUTE_INPUT", &unexpected),
            explain_unexpected("ROUTE_INPUT", &CommandResult::Ok),
            unexpected_reply("SPAWN_TERMINAL"),
            unexpected_reply("HELLO"),
        ] {
            assert!(!sentence.contains('{'), "{sentence}");
            assert!(!sentence.contains("OkWith"), "{sentence}");
        }
    }

    /// The label is the variant name as lowercase spaced words — never the
    /// CamelCase `Debug` render.
    #[test]
    fn error_code_labels_are_spaced_lowercase() {
        assert_eq!(
            error_code_label(ErrorCode::TerminalNotFound),
            "terminal not found"
        );
        assert_eq!(
            error_code_label(ErrorCode::PermissionDenied),
            "permission denied"
        );
        assert_eq!(error_code_label(ErrorCode::InternalError), "internal error");
        assert_eq!(
            error_code_label(ErrorCode::UnsupportedSatelliteRoute),
            "unsupported satellite route"
        );
    }
}
