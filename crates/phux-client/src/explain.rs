//! Sentences for server replies the client did not expect.
//!
//! The server's own `Error` message verbatim, or, for a reply kind this
//! client cannot interpret, a version-skew pointer to `phux doctor`; never a
//! `Debug` dump of a wire enum.

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};

/// Explain a [`CommandResult`] a caller did not expect, attributed to `verb`:
/// `"{verb} failed: {message}"` for an `Error`, else [`unexpected_reply`].
#[must_use]
pub fn explain_unexpected(verb: &str, result: &CommandResult) -> String {
    match result {
        CommandResult::Error { message, .. } => format!("{verb} failed: {message}"),
        _ => unexpected_reply(verb),
    }
}

/// The sentence for a reply kind this client cannot interpret: likely
/// version skew, naming `phux doctor` and this client's protocol triple.
#[must_use]
pub fn unexpected_reply(verb: &str) -> String {
    format!(
        "unexpected {verb} reply; client and server versions may differ - \
         run `phux doctor` (client protocol {}.{}.{})",
        PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor, PROTOCOL_VERSION.patch,
    )
}

/// A wire [`ErrorCode`] as lowercase spaced words (`TerminalNotFound` becomes
/// `terminal not found`), derived from the variant name so codes a newer
/// protocol adds render too.
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

    /// An unknown reply kind names `phux doctor` and the protocol triple,
    /// and never leaks a `Debug` render.
    #[test]
    fn unknown_kind_names_doctor_and_the_protocol_triple() {
        let triple = format!(
            "client protocol {}.{}.{}",
            PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor, PROTOCOL_VERSION.patch,
        );
        for sentence in [
            explain_unexpected(
                "GET_STATE",
                &CommandResult::OkWith(CommandValue::Bytes(vec![1, 2, 3])),
            ),
            explain_unexpected("GET_STATE", &CommandResult::Ok),
        ] {
            assert!(
                sentence.contains("unexpected GET_STATE reply"),
                "{sentence}"
            );
            assert!(sentence.contains("run `phux doctor`"), "{sentence}");
            assert!(sentence.contains(&triple), "{sentence}");
            assert!(!sentence.contains('{'), "{sentence}");
        }
    }

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
