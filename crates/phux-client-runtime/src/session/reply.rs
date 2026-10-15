//! A correlated request's answer, and the words a refusal reaches users in.

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::wire::frame::{CommandResult, ErrorCode, FrameKind};

/// The peer's correlated `ERROR` answer to one request (proto.md §9). Its
/// `Display` renders the code as words, since it reaches users verbatim.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}: {message}", error_code_label(*.code))]
pub struct Refusal {
    /// The typed code the peer refused with.
    pub code: ErrorCode,
    /// The peer's human-readable explanation.
    pub message: String,
}

/// A peer's answer to one correlated request: the reply payload, or the
/// [`Refusal`] it answered with instead.
pub type Answer<T> = Result<T, Refusal>;

/// One correlated round trip's answer plus every frame the peer pushed ahead
/// of it. Opaque on purpose: the fields are reachable only through methods,
/// so neither half can be destructured away by omission.
#[derive(Debug)]
#[must_use = "the reply carries frames the server will never re-send; dropping \
              it loses them"]
pub struct Reply<T = CommandResult> {
    /// The pair's answer: its reply-frame payload, or the peer's refusal.
    result: T,
    /// Frames observed while waiting, in arrival order.
    interleaved: Vec<FrameKind>,
}

impl<T> Reply<T> {
    /// A reply from its answer and the frames that arrived ahead of it.
    pub(crate) const fn new(result: T, interleaved: Vec<FrameKind>) -> Self {
        Self {
            result,
            interleaved,
        }
    }

    /// The answer and the interleaved frames, in arrival order.
    #[must_use]
    pub fn into_parts(self) -> (T, Vec<FrameKind>) {
        (self.result, self.interleaved)
    }

    /// Borrow the answer without consuming the reply.
    #[must_use]
    pub const fn result(&self) -> &T {
        &self.result
    }

    /// Borrow the frames the server interleaved ahead of the answer.
    #[must_use]
    pub fn interleaved(&self) -> &[FrameKind] {
        &self.interleaved
    }

    /// Take the answer and drop the interleaved frames.
    ///
    /// Only correct when the server provably pushes nothing before the
    /// answer (no frame from the handler, no subscription on the
    /// connection); cite the handler at each call site. A non-empty drop is
    /// logged at `warn`.
    #[must_use]
    pub fn into_result_ignoring_interleaved(self) -> T {
        if !self.interleaved.is_empty() {
            tracing::warn!(
                dropped = self.interleaved.len(),
                frames = ?self.interleaved,
                "correlated reply discarded frames the server interleaved ahead \
                 of the answer; the server will not re-send them",
            );
        }
        self.result
    }
}

/// The correlated `ERROR` that answers `request_id`, if `frame` is one
/// (proto.md §9). An uncorrelated `ERROR` is a pushed notice, not an answer.
pub(super) fn refusal_for(frame: &FrameKind, request_id: u32) -> Option<Refusal> {
    match frame {
        FrameKind::Error {
            request_id: Some(got),
            code,
            message,
        } if *got == request_id => Some(Refusal {
            code: *code,
            message: message.clone(),
        }),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Users see refusals verbatim, so the code renders as words, not Debug.
    #[test]
    fn refusal_display_renders_the_code_as_words_not_debug() {
        let refused = Refusal {
            code: ErrorCode::TerminalNotFound,
            message: "pane @9 does not exist".to_owned(),
        };
        assert_eq!(
            refused.to_string(),
            "terminal not found: pane @9 does not exist"
        );
    }

    #[test]
    fn only_an_error_carrying_the_request_id_answers_it() {
        let error = |request_id| FrameKind::Error {
            request_id,
            code: ErrorCode::InvalidCommand,
            message: "no".to_owned(),
        };
        assert!(refusal_for(&error(Some(7)), 7).is_some());
        assert!(refusal_for(&error(Some(8)), 7).is_none());
        assert!(refusal_for(&error(None), 7).is_none());
        assert!(refusal_for(&FrameKind::Pong { nonce: 7 }, 7).is_none());
    }

    #[test]
    fn unexpected_reply_names_the_verb_and_the_remedy() {
        let sentence = unexpected_reply("HELLO");
        assert!(sentence.starts_with("unexpected HELLO reply"), "{sentence}");
        assert!(sentence.contains("phux doctor"), "{sentence}");
    }
}
