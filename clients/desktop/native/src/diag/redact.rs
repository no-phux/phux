//! Credential redaction for log lines and diagnostic receipts.
//!
//! Three passes, each conservative about what it hides:
//!
//! - **URL userinfo** with a password (`wss://user:secret@host`) loses the
//!   whole userinfo. A bare user (`ssh://alice@box`) is a route, not a secret.
//! - **Labeled values** (`token=…`, `secret: …`, `Authorization: Bearer …`)
//!   lose the value after the label.
//! - **Long opaque runs**: 40 or more characters of the base64 or base64url
//!   alphabet that mix letters and digits, the shape of a pairing token, a
//!   key line or a JWK member. Server ids (32 hex) and paths stay readable.

/// Shortest opaque run [`redact`] hides; the same bound the CLI uses.
const OPAQUE_MIN_RUN: usize = 40;

/// What a hidden value reads as.
pub const REDACTED: &str = "<redacted>";

/// Labels whose value is a secret. Matched case-insensitively at a word start.
const SECRET_LABELS: &[&str] = &[
    "authorization",
    "bearer",
    "token",
    "secret",
    "password",
    "passwd",
    "psk",
    "api_key",
    "apikey",
    "private_key",
];

/// Scheme words that introduce the credential after them.
const AUTH_SCHEMES: &[&str] = &["bearer", "basic", "token"];

/// `text` with credential-shaped values replaced by [`REDACTED`].
#[must_use]
pub fn redact(text: &str) -> String {
    let text = redact_userinfo(text);
    let text = redact_labeled(&text);
    redact_opaque_runs(&text)
}

// ── URL userinfo ───────────────────────────────────────────────────────

fn redact_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail
            .find(|ch: char| ch == '/' || ch.is_whitespace() || is_quote(ch))
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(split) if authority[..split].contains(':') => {
                out.push_str(REDACTED);
                out.push_str(&authority[split..]);
            }
            _ => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

// ── Labeled values ─────────────────────────────────────────────────────

fn redact_labeled(text: &str) -> String {
    // ASCII lowercase keeps byte offsets, so spans found in `lower` index `text`.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    while at < text.len() {
        match labeled_value(&lower, at) {
            Some((start, end)) => {
                out.push_str(&text[copied..start]);
                out.push_str(REDACTED);
                copied = end;
                at = end;
            }
            None => at += lower[at..].chars().next().map_or(1, char::len_utf8),
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// The byte span of a secret value whose label starts at `at`, if one does.
fn labeled_value(lower: &str, at: usize) -> Option<(usize, usize)> {
    if !word_start(lower, at) {
        return None;
    }
    let label = SECRET_LABELS
        .iter()
        .find(|label| lower[at..].starts_with(*label))?;
    let after_label = skip_while(lower, at + label.len(), is_label_char);
    let after_quote = skip_while(lower, after_label, is_quote);
    let after_space = skip_while(lower, after_quote, is_inline_space);
    let separated = lower[after_space..].starts_with(['=', ':']);
    let start = if separated {
        skip_while(lower, after_space + 1, |ch| {
            is_inline_space(ch) || is_quote(ch)
        })
    } else if *label == "bearer" && after_space > after_quote {
        after_space
    } else {
        return None;
    };
    let start = skip_auth_scheme(lower, start);
    let end = skip_while(lower, start, is_value_char);
    (end > start).then_some((start, end))
}

/// `Authorization: Bearer abc` hides `abc`, not the scheme word.
fn skip_auth_scheme(lower: &str, start: usize) -> usize {
    let word_end = skip_while(lower, start, is_value_char);
    let word = &lower[start..word_end];
    if !AUTH_SCHEMES.contains(&word) {
        return start;
    }
    let next = skip_while(lower, word_end, is_inline_space);
    if next > word_end { next } else { start }
}

/// A label starts a word: `box.token: x` names a file, not a token.
fn word_start(text: &str, at: usize) -> bool {
    text[..at].chars().next_back().is_none_or(|ch| {
        ch.is_whitespace() || is_quote(ch) || matches!(ch, '{' | '(' | '[' | ',' | ';' | '&' | '?')
    })
}

fn skip_while(text: &str, from: usize, keep: impl Fn(char) -> bool) -> usize {
    text[from..]
        .char_indices()
        .find(|&(_, ch)| !keep(ch))
        .map_or(text.len(), |(offset, _)| from + offset)
}

fn is_label_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

fn is_quote(ch: char) -> bool {
    matches!(ch, '"' | '\'' | '`')
}

fn is_inline_space(ch: char) -> bool {
    ch == ' ' || ch == '\t'
}

fn is_value_char(ch: char) -> bool {
    !ch.is_whitespace() && !is_quote(ch) && !matches!(ch, ',' | ';' | ')' | ']' | '}' | '&')
}

// ── Opaque runs ────────────────────────────────────────────────────────

fn redact_opaque_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for ch in text.chars() {
        if is_opaque_char(ch) {
            run.push(ch);
            continue;
        }
        flush_run(&mut out, &mut run);
        out.push(ch);
    }
    flush_run(&mut out, &mut run);
    out
}

/// The base64 and base64url alphabets, minus `/`, so a path stays a path.
fn is_opaque_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '+' | '=' | '-' | '_')
}

fn flush_run(out: &mut String, run: &mut String) {
    if looks_like_credential(run) {
        out.push_str(REDACTED);
    } else {
        out.push_str(run);
    }
    run.clear();
}

/// Long and mixed: a long identifier made only of words is not a key.
fn looks_like_credential(run: &str) -> bool {
    run.len() >= OPAQUE_MIN_RUN
        && run.bytes().any(|byte| byte.is_ascii_digit())
        && run.bytes().any(|byte| byte.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "3f9c0a7be41d58c26a9f0e1b7d4c3a2958e6f1b0c9d8a7e6f5a4b3c2d1e0f9a8";

    #[test]
    fn a_pairing_token_is_hidden_wherever_it_appears() {
        let line = format!("dial wss://box:8787 with {TOKEN} failed");
        let redacted = redact(&line);
        assert!(!redacted.contains(TOKEN), "{redacted}");
        assert_eq!(
            redacted,
            format!("dial wss://box:8787 with {REDACTED} failed")
        );
    }

    #[test]
    fn labeled_values_lose_the_value_not_the_label() {
        assert_eq!(
            redact("relay token=hunter2 refused"),
            format!("relay token={REDACTED} refused")
        );
        assert_eq!(
            redact(r#"{"secret": "s3cr3t", "name": "box"}"#),
            format!(r#"{{"secret": "{REDACTED}", "name": "box"}}"#)
        );
        assert_eq!(
            redact("Authorization: Bearer abc.def"),
            format!("Authorization: Bearer {REDACTED}")
        );
        assert_eq!(redact("sent Bearer xyz"), format!("sent Bearer {REDACTED}"));
        assert_eq!(
            redact("PASSWORD=p4ss api_key: k"),
            format!("PASSWORD={REDACTED} api_key: {REDACTED}")
        );
    }

    #[test]
    fn registry_endpoint_passwords_are_hidden_but_routes_stay() {
        assert_eq!(
            redact("dial wss://user@realm:synthetic-secret@host:8787/x"),
            format!("dial wss://{REDACTED}@host:8787/x")
        );
        assert_eq!(redact("ssh://alice@box:22"), "ssh://alice@box:22");
        assert_eq!(redact("quic://host:8788"), "quic://host:8788");
    }

    #[test]
    fn ordinary_diagnostics_are_untouched() {
        for line in [
            "connection Attached -> Reconnecting epoch=3 error=None",
            "pairing token refused",
            "could not read token file /Users/me/.config/phux/box.token: denied",
            "server 0123456789abcdef0123456789abcdef attached",
            "phux_desktop::connection::reconnect_after_handoff_completed_cleanly",
            "/Users/me/Library/Application/Support/phux/very/deep/path/0123456789",
        ] {
            assert_eq!(redact(line), line);
        }
    }

    #[test]
    fn non_ascii_text_is_preserved() {
        assert_eq!(
            redact("café token=x ok"),
            format!("café token={REDACTED} ok")
        );
    }
}
