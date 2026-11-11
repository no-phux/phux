#![allow(clippy::expect_used, reason = "tests")]

use super::{AnswerSource, live_ask, select_answer};
use phux_client::ask::AskMarker;

fn marker(id: &str, suggestions: &[&str]) -> AskMarker {
    AskMarker {
        id: id.to_owned(),
        question: "Deploy to prod?".to_owned(),
        suggestions: suggestions.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// The failure this verb exists to prevent: the agent moved on, so the
/// answer must not land in whatever replaced the question.
#[test]
fn an_id_the_pane_has_moved_past_is_refused_as_stale() {
    let marker = live_ask(Some("phux-ask[deploy]:Deploy to prod??s=Yes|No"), "deploy")
        .expect("a live sentinel with the named id is answerable");
    assert_eq!(marker.suggestions, ["Yes", "No"]);

    let refusal = live_ask(Some("phux-ask[migrate]:Run migrations??s=Yes|No"), "deploy")
        .expect_err("a different live question must not be answered");
    assert_eq!(refusal.err.code, super::codes::ASK_STALE);
    assert_eq!(refusal.exit, crate::exit_codes::EXIT_USAGE);
    // The diagnostic must name BOTH ids, or the caller cannot tell a
    // stale read from a typo.
    assert!(
        refusal.err.message.contains("migrate"),
        "{}",
        refusal.err.message
    );
    assert!(
        refusal.err.message.contains("deploy"),
        "{}",
        refusal.err.message
    );
}

/// No sentinel at all, and the degenerate "pane went away" case: both are
/// "there is nothing to answer", never "answer it anyway".
#[test]
fn a_pane_with_no_ask_is_refused() {
    for title in [Some("vim README.md"), Some(""), None] {
        let refusal =
            live_ask(title, "deploy").expect_err("a pane that is not asking has no answer");
        assert_eq!(refusal.err.code, super::codes::NO_ACTIVE_ASK);
        assert_eq!(refusal.exit, crate::exit_codes::EXIT_USAGE);
    }
}

/// An anonymous ask cannot be correlated, so it is refused rather than
/// answered on the strength of its text alone.
#[test]
fn an_ask_with_no_id_is_refused_rather_than_matched_on_text() {
    let refusal = live_ask(Some("phux-ask:Deploy to prod??s=Yes|No"), "deploy")
        .expect_err("an anonymous ask must not be answerable");
    assert_eq!(refusal.err.code, super::codes::ASK_UNIDENTIFIED);
}

/// `--choice N` is 1-based and sends the published string verbatim — the
/// central contract of the verb.
#[test]
fn choice_sends_the_published_suggestion_verbatim() {
    let ask = marker("deploy", &["Yes", "No", "Hold"]);
    let (answer, source) = select_answer(&ask, Some(3), None, false).expect("choice 3 is in range");
    assert_eq!(answer, "Hold");
    assert_eq!(source, AnswerSource::Choice);
}

/// Out-of-range and 0 are refused, and the diagnostic lists what the valid
/// choices actually were.
#[test]
fn an_out_of_range_choice_is_refused_and_lists_the_options() {
    let ask = marker("deploy", &["Yes", "No"]);
    for index in [0, 3, 99] {
        let refusal =
            select_answer(&ask, Some(index), None, false).expect_err("only 1..=2 are choices here");
        assert_eq!(refusal.err.code, super::codes::CHOICE_OUT_OF_RANGE);
        assert!(
            refusal.err.message.contains("1) Yes"),
            "{}",
            refusal.err.message
        );
        assert!(
            refusal.err.message.contains("2) No"),
            "{}",
            refusal.err.message
        );
    }
}

/// `--text` matching a suggestion is accepted, trimmed and
/// case-insensitively: shell quoting must not read as a different answer.
#[test]
fn listed_text_is_accepted_trimmed_and_case_insensitively() {
    let ask = marker("deploy", &["Yes", "No"]);
    let (answer, source) =
        select_answer(&ask, None, Some(" yes "), false).expect("a listed answer is valid");
    // The caller's spelling is what gets typed — phux does not silently
    // rewrite it to the suggestion's casing.
    assert_eq!(answer, " yes ");
    assert_eq!(source, AnswerSource::Text);
}

/// The default refusal for a free-form answer to a closed set, and the
/// explicit override.
#[test]
fn unlisted_text_is_refused_unless_explicitly_allowed() {
    let ask = marker("deploy", &["Yes", "No"]);
    let refusal = select_answer(&ask, None, Some("maybe later"), false)
        .expect_err("an unlisted answer to a closed set is refused by default");
    assert_eq!(refusal.err.code, super::codes::UNLISTED_ANSWER);
    assert!(
        refusal.err.message.contains("1) Yes"),
        "{}",
        refusal.err.message
    );

    let (answer, source) = select_answer(&ask, None, Some("maybe later"), true)
        .expect("--allow-unlisted is the explicit override");
    assert_eq!(answer, "maybe later");
    assert_eq!(source, AnswerSource::Unlisted);
}

/// An ask that published no suggestions has nothing to validate against,
/// so free text is the only thing it can take — no flag required.
#[test]
fn an_open_ask_takes_free_text_without_the_override() {
    let ask = marker("deploy", &[]);
    let (answer, source) =
        select_answer(&ask, None, Some("ship it"), false).expect("an open ask takes prose");
    assert_eq!(answer, "ship it");
    assert_eq!(source, AnswerSource::Text);
}

/// Answers that cannot be typed safely, and choices against an ask with no
/// suggestions, are refused before anything is typed.
#[test]
fn unanswerable_input_is_refused_before_anything_is_typed() {
    let open = marker("deploy", &[]);
    let long = "x".repeat(super::MAX_ANSWER_BYTES + 1);
    let cases: [(&AskMarker, Option<usize>, Option<&str>, &str); 8] = [
        (&open, Some(1), None, super::codes::NO_SUGGESTIONS),
        (&open, None, None, super::codes::NO_ANSWER),
        (&open, None, Some("yes\nno"), super::codes::INVALID_ANSWER),
        (&open, None, Some("yes\r\nno"), super::codes::INVALID_ANSWER),
        (&open, None, Some("yes\r"), super::codes::INVALID_ANSWER),
        (&open, None, Some(""), super::codes::INVALID_ANSWER),
        (&open, None, Some("   "), super::codes::INVALID_ANSWER),
        (&open, None, Some(&long), super::codes::INVALID_ANSWER),
    ];
    for (ask, choice, text, code) in cases {
        let refusal = select_answer(ask, choice, text, false)
            .expect_err("unanswerable input must be refused");
        assert_eq!(refusal.err.code, code, "{choice:?} {text:?}");
    }
}

/// Exit-code contract for the server's answers, keyed on the wire code so
/// it pins `agent_prompt::classify` plus this verb's mapping. Delivery
/// unknown and internal errors are exit 3: bytes may have landed.
#[test]
fn server_refusals_map_to_the_documented_exit_codes() {
    use phux_client::agent_prompt::classify;
    use phux_protocol::wire::frame::{CommandResult, ErrorCode};
    let cases = [
        (
            ErrorCode::InputDeliveryUnknown,
            crate::exit_codes::EXIT_PARTIAL_VIEW,
        ),
        (
            ErrorCode::InternalError,
            crate::exit_codes::EXIT_PARTIAL_VIEW,
        ),
        (
            ErrorCode::ResourceExhausted,
            crate::exit_codes::EXIT_FAILURE,
        ),
        (ErrorCode::InputNotWritten, crate::exit_codes::EXIT_FAILURE),
        (ErrorCode::TerminalNotFound, crate::exit_codes::EXIT_FAILURE),
        (ErrorCode::InvalidCommand, crate::exit_codes::EXIT_USAGE),
        (ErrorCode::InputLeaseHeld, crate::exit_codes::EXIT_USAGE),
        (
            ErrorCode::CanonicalLimitExceeded,
            crate::exit_codes::EXIT_USAGE,
        ),
        (ErrorCode::UnsafePaste, crate::exit_codes::EXIT_USAGE),
    ];
    for (code, exit) in cases {
        let verdict = classify(&CommandResult::Error {
            code,
            message: "diagnostic".to_owned(),
        });
        let refusal = super::refusal_for_verdict(verdict, "@7");
        assert_eq!(refusal.exit, exit, "{code:?}");
        assert!(refusal.err.message.contains("@7"), "{code:?}");
    }
    // The one outcome a caller must never retry says so in its remedy.
    let unknown = super::refusal_for_verdict(super::ApplyVerdict::Unknown("why".to_owned()), "@7");
    assert_eq!(unknown.err.code, super::codes::ANSWER_DELIVERY_UNKNOWN);
    assert!(
        unknown.err.remedy.contains("do NOT retry"),
        "{}",
        unknown.err.remedy
    );
}
