//! The `phux-ask` title marker an in-pane agent sets.

/// Title prefix an in-pane agent sets (OSC 0/2) to ask a human a question:
///
/// ```text
/// ESC ] 2 ; phux-ask:<question>                         ST
/// ESC ] 2 ; phux-ask[<id>]:<question>                   ST
/// ESC ] 2 ; phux-ask[<id>]:<question>?s=opt1|opt2|opt3  ST
/// ```
///
/// Retitling away from a `phux-ask` title clears the ask.
pub(super) const ASK_TITLE_PREFIX: &str = "phux-ask";

/// A parsed `phux-ask` title marker. Equality is by content so the actor can
/// edge-filter re-asserted markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AskMarker {
    /// Correlation id; empty when the title omits `[id]`.
    pub(super) id: String,
    /// The question text presented to the human.
    pub(super) question: String,
    /// Suggested answers, in presentation order; empty when none were given.
    pub(super) suggestions: Vec<String>,
}

impl AskMarker {
    /// Parse a `phux-ask` title (see [`ASK_TITLE_PREFIX`]); `None` for any
    /// other title, including a bare `phux-ask` without `:`.
    pub(super) fn parse(title: &str) -> Option<Self> {
        let rest = title.strip_prefix(ASK_TITLE_PREFIX)?;
        // Optional `[id]` segment immediately after the prefix.
        let (id, rest) = if let Some(after_bracket) = rest.strip_prefix('[') {
            let close = after_bracket.find(']')?;
            (
                after_bracket[..close].to_owned(),
                &after_bracket[close + 1..],
            )
        } else {
            (String::new(), rest)
        };
        // The question is introduced by ':'. Without it there is no ask.
        let body = rest.strip_prefix(':')?;
        // Optional `?s=opt1|opt2` suggestion suffix.
        let (question, suggestions) = match body.split_once("?s=") {
            Some((q, sugg)) => (
                q.to_owned(),
                sugg.split('|')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            ),
            None => (body.to_owned(), Vec::new()),
        };
        Some(Self {
            id,
            question,
            suggestions,
        })
    }
}
