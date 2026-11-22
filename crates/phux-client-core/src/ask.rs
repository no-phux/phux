//! The client-side reader of the ADR-0035 `phux-ask` title sentinel.
//!
//! What a pane is asking right now, and the id an answer must correlate
//! against. Shared by the CLI's `agent answer` and the runtime's
//! acknowledged answer path so both refuse the same stale asks.

/// Literal prefix of the ADR-0035 `phux-ask` terminal-title sentinel.
const ASK_TITLE_PREFIX: &str = "phux-ask";

/// A pending ask, as read out of a pane's terminal title: the client-side
/// reader of the ADR-0035 grammar the server's own `AskMarker` parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskMarker {
    /// Stable question id an answer correlates against. Empty when the title
    /// omits the `[id]` segment — see [`AskMarker::is_identified`].
    pub id: String,
    /// The question text presented to the human.
    pub question: String,
    /// Suggested answers, in presentation order; empty when none were given.
    pub suggestions: Vec<String>,
}

impl AskMarker {
    /// Whether this ask carries an id an answer can be correlated against;
    /// an anonymous ask is indistinguishable from the next one worded alike.
    #[must_use]
    pub const fn is_identified(&self) -> bool {
        !self.id.is_empty()
    }

    /// The 1-based `index`'th suggestion, or `None` when out of range.
    #[must_use]
    pub fn suggestion(&self, index: usize) -> Option<&str> {
        index
            .checked_sub(1)
            .and_then(|zero_based| self.suggestions.get(zero_based))
            .map(String::as_str)
    }

    /// Whether `answer` is one of the published suggestions, compared trimmed
    /// and case-insensitively (a shell quoting artifact must not read as a
    /// different answer).
    #[must_use]
    pub fn lists(&self, answer: &str) -> bool {
        self.suggestions
            .iter()
            .any(|suggestion| suggestion.trim().eq_ignore_ascii_case(answer.trim()))
    }
}

/// Parse the ADR-0035 ask sentinel out of a pane's terminal title, or `None`
/// when the title is not one.
///
/// Grammar: `phux-ask`, an optional `[<id>]`, `:`, the question, then an
/// optional `?s=opt1|opt2` suggestion suffix. A bare `phux-ask` with no `:`
/// is not a marker (it carries no question).
#[must_use]
pub fn parse_ask_title(title: &str) -> Option<AskMarker> {
    let rest = title.strip_prefix(ASK_TITLE_PREFIX)?;
    let (id, rest) = if let Some(after_bracket) = rest.strip_prefix('[') {
        let close = after_bracket.find(']')?;
        (
            after_bracket[..close].to_owned(),
            &after_bracket[close + 1..],
        )
    } else {
        (String::new(), rest)
    };
    let body = rest.strip_prefix(':')?;
    let (question, suggestions) = match body.split_once("?s=") {
        Some((question, suggestions)) => (
            question.to_owned(),
            suggestions
                .split('|')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        None => (body.to_owned(), Vec::new()),
    };
    Some(AskMarker {
        id,
        question,
        suggestions,
    })
}

/// Longest answer the guarded answer path will type, in bytes (the ask
/// side's own ceiling); longer is prose, for `phux agent prompt`.
pub const MAX_ANSWER_BYTES: usize = 4096;

/// Why `text` cannot be typed as an answer. A line break becomes an extra
/// submission on a pane without bracketed paste, which no client can
/// observe, so it is refused outright.
#[must_use]
pub fn answer_text_refusal(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return Some("the answer is empty".to_owned());
    }
    if text.contains('\n') || text.contains('\r') {
        return Some("the answer contains a line break".to_owned());
    }
    if text.len() > MAX_ANSWER_BYTES {
        return Some(format!(
            "the answer is {} bytes; the limit is {MAX_ANSWER_BYTES}",
            text.len()
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{answer_text_refusal, parse_ask_title};

    /// The full sentinel: id, question, and the published suggestion set.
    #[test]
    fn a_full_sentinel_parses_into_id_question_and_suggestions() {
        let marker = parse_ask_title("phux-ask[deploy]:Deploy to prod??s=Yes|No|Hold")
            .expect("a full sentinel is a marker");
        assert_eq!(marker.id, "deploy");
        assert_eq!(marker.question, "Deploy to prod?");
        assert_eq!(marker.suggestions, ["Yes", "No", "Hold"]);
        assert!(marker.is_identified());
        assert_eq!(marker.suggestion(2), Some("No"));
        assert_eq!(marker.suggestion(4), None);
        // 1-based: index 0 must not silently mean the first suggestion.
        assert_eq!(marker.suggestion(0), None);
        assert!(marker.lists(" yes "));
        assert!(!marker.lists("maybe"));
    }

    /// An ask with no `[id]` parses, but is not answerable: the caller has to
    /// be able to tell "still that question" from "a new one worded the same".
    #[test]
    fn an_anonymous_sentinel_parses_but_is_not_identified() {
        let marker = parse_ask_title("phux-ask:Continue?").expect("an id is optional");
        assert_eq!(marker.id, "");
        assert_eq!(marker.question, "Continue?");
        assert!(marker.suggestions.is_empty());
        assert!(!marker.is_identified());
    }

    /// Everything that is not a sentinel reads as "no live ask" — including
    /// the degenerate prefix with no question after it.
    #[test]
    fn non_sentinel_titles_are_not_markers() {
        for title in [
            "",
            "vim README.md",
            "phux-ask",
            "phux-ask[q1]",
            "a phux-ask[q1]:Continue?",
            "phux-asked[q1]:Continue?",
        ] {
            assert!(
                parse_ask_title(title).is_none(),
                "'{title}' must not read as a live ask"
            );
        }
    }

    #[test]
    fn unsafe_answers_are_refused_before_any_write() {
        assert!(answer_text_refusal("yes").is_none());
        assert!(answer_text_refusal("  ").is_some());
        assert!(answer_text_refusal("yes\nno").is_some());
        assert!(answer_text_refusal(&"x".repeat(4097)).is_some());
    }
}
