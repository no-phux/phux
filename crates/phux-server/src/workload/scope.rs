//! The registry's human scope grammar, validated locally.
//!
//! A registry scope string is `"<verb>[,<verb>...]@<selector>"` where a verb
//! is one of `inventory`, `observe`, `create`, `bind`, `input`, `signal`, or
//! the single wildcard `*`, and a selector is `global`, `host`,
//! `host:<name>`, `group:<u32>`, `terminal:<u32>`, or
//! `terminal:<host>/<u32>` (`workload-auth.md` §5; PHA-406 phase 2 P1.1).
//!
//! This module only decides whether a string is well formed. It is one
//! function on purpose: the scope-enforcement lane replaces it with the
//! canonical `phux_protocol::scope` parser, and nothing else here should need
//! to change when it does. Error messages never echo the rejected input, so a
//! secret pasted into the wrong argument cannot reach stderr through them.

/// The closed verb names a scope string may carry.
const VERBS: [&str; 6] = ["inventory", "observe", "create", "bind", "input", "signal"];

/// The wildcard verb: every verb, and only on its own.
const ALL_VERBS: &str = "*";

/// Longest host a selector may name, in bytes (`workload-auth.md` §5).
const MAX_HOST_BYTES: usize = 255;

/// Why a scope string was refused. The message names the rule, never the
/// input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopeGrammarError {
    /// Not of the form `<verbs>@<selector>`.
    #[error("a scope must have the form <verb>[,<verb>...]@<selector>")]
    Shape,
    /// A verb outside the closed set.
    #[error("a scope names a verb outside inventory|observe|create|bind|input|signal|*")]
    UnknownVerb,
    /// A verb listed twice, or `*` combined with named verbs.
    #[error("a scope repeats a verb or combines `*` with named verbs")]
    RedundantVerb,
    /// A selector outside the closed set.
    #[error(
        "a scope names a selector outside global|host|host:<name>|group:<id>|terminal:<id>|terminal:<host>/<id>"
    )]
    UnknownSelector,
    /// A host that is empty, too long, or carries a control character.
    #[error("a scope host must be 1..=255 bytes with no NUL or control character")]
    InvalidHost,
    /// An id that is not a canonical unsigned 32-bit decimal.
    #[error("a scope id must be a canonical unsigned 32-bit decimal")]
    InvalidId,
}

/// Check one registry scope string against the grammar.
///
/// # Errors
///
/// Returns the first [`ScopeGrammarError`] rule the string breaks.
pub fn validate_scope(scope: &str) -> Result<(), ScopeGrammarError> {
    let (verbs, selector) = scope.split_once('@').ok_or(ScopeGrammarError::Shape)?;
    validate_verbs(verbs)?;
    validate_selector(selector)
}

fn validate_verbs(verbs: &str) -> Result<(), ScopeGrammarError> {
    if verbs == ALL_VERBS {
        return Ok(());
    }
    let mut seen = [false; VERBS.len()];
    for verb in verbs.split(',') {
        if verb == ALL_VERBS {
            return Err(ScopeGrammarError::RedundantVerb);
        }
        let index = VERBS
            .iter()
            .position(|known| *known == verb)
            .ok_or(ScopeGrammarError::UnknownVerb)?;
        if std::mem::replace(&mut seen[index], true) {
            return Err(ScopeGrammarError::RedundantVerb);
        }
    }
    Ok(())
}

fn validate_selector(selector: &str) -> Result<(), ScopeGrammarError> {
    if matches!(selector, "global" | "host") {
        return Ok(());
    }
    match selector.split_once(':') {
        Some(("host", name)) => validate_host(name),
        Some(("group", id)) => validate_id(id),
        Some(("terminal", target)) => validate_terminal(target),
        _ => Err(ScopeGrammarError::UnknownSelector),
    }
}

/// `terminal:<u32>` names a local Terminal; `terminal:<host>/<u32>` a
/// satellite one. The id never contains `/`, so the last one splits.
fn validate_terminal(target: &str) -> Result<(), ScopeGrammarError> {
    match target.rsplit_once('/') {
        Some((host, id)) => {
            validate_host(host)?;
            validate_id(id)
        }
        None => validate_id(target),
    }
}

fn validate_host(host: &str) -> Result<(), ScopeGrammarError> {
    let sized = (1..=MAX_HOST_BYTES).contains(&host.len());
    if sized && !host.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(ScopeGrammarError::InvalidHost)
    }
}

/// One spelling per value: digits only, no sign, no leading zero.
fn validate_id(id: &str) -> Result<(), ScopeGrammarError> {
    let digits = !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit());
    let minimal = id == "0" || !id.starts_with('0');
    if digits && minimal && id.parse::<u32>().is_ok() {
        Ok(())
    } else {
        Err(ScopeGrammarError::InvalidId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_selector_form_and_verb_list_parses() {
        for scope in [
            "*@global",
            "inventory@host",
            "observe,input@host:devbox",
            "bind@group:0",
            "signal,create@group:4294967295",
            "inventory,observe,create,bind,input,signal@terminal:7",
            "observe@terminal:devbox/12",
            "input@host:a:b",
            "observe@terminal:a/b/3",
        ] {
            assert_eq!(validate_scope(scope), Ok(()), "{scope}");
        }
    }

    #[test]
    fn malformed_scopes_are_refused_by_rule() {
        let cases = [
            ("", ScopeGrammarError::Shape),
            ("terminal.control", ScopeGrammarError::Shape),
            ("@global", ScopeGrammarError::UnknownVerb),
            ("observe,@global", ScopeGrammarError::UnknownVerb),
            ("Observe@global", ScopeGrammarError::UnknownVerb),
            (" observe@global", ScopeGrammarError::UnknownVerb),
            ("observe,observe@global", ScopeGrammarError::RedundantVerb),
            ("*,observe@global", ScopeGrammarError::RedundantVerb),
            ("observe@", ScopeGrammarError::UnknownSelector),
            ("observe@everything", ScopeGrammarError::UnknownSelector),
            ("observe@global ", ScopeGrammarError::UnknownSelector),
            ("observe@host:", ScopeGrammarError::InvalidHost),
            ("observe@host:a\u{0}b", ScopeGrammarError::InvalidHost),
            ("observe@host:a\nb", ScopeGrammarError::InvalidHost),
            ("observe@group:", ScopeGrammarError::InvalidId),
            ("observe@group:07", ScopeGrammarError::InvalidId),
            ("observe@group:+7", ScopeGrammarError::InvalidId),
            ("observe@group:4294967296", ScopeGrammarError::InvalidId),
            ("observe@terminal:/3", ScopeGrammarError::InvalidHost),
            ("observe@terminal:devbox/", ScopeGrammarError::InvalidId),
        ];
        for (scope, expected) in cases {
            assert_eq!(validate_scope(scope), Err(expected), "{scope:?}");
        }
        let long_host = format!("observe@host:{}", "h".repeat(MAX_HOST_BYTES + 1));
        assert_eq!(
            validate_scope(&long_host),
            Err(ScopeGrammarError::InvalidHost)
        );
    }

    #[test]
    fn refusals_never_echo_the_input() {
        let secret = "-----BEGIN PRIVATE KEY-----MIIEvQ";
        let error = validate_scope(secret).unwrap_err();
        assert!(!error.to_string().contains("MIIEvQ"));
        assert!(!format!("{error:?}").contains("MIIEvQ"));
    }
}
