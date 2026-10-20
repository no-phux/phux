//! Claude Code: the OAuth usage endpoint, authorized by the token Claude
//! Code keeps in the macOS Keychain (`Claude Code-credentials`) or in
//! `~/.claude/.credentials.json`. The token is read, never refreshed:
//! an expired one makes the plan `unavailable` until Claude Code runs.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::model::{Plan, Status, Window};
use crate::state::SourceState;
use crate::util::{fingerprint, iso_ms, number, text};
use crate::{Failure, Request, Sources, identity, polled, with_last_good};

/// State key.
pub(crate) const SOURCE: &str = "claude";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const BETA: &str = "oauth-2025-04-20";
/// The endpoint answers unknown agents with sticky 429s; keep this a
/// plausible Claude Code version.
const USER_AGENT: &str = "claude-code/2.1.205";
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// The known windows, in display order: `(field, id, label, minutes)`.
const WINDOWS: [(&str, &str, &str, u32); 4] = [
    ("five_hour", "5h", "5h", 300),
    ("seven_day", "7d", "7d", 10_080),
    ("seven_day_opus", "7d_opus", "7d opus", 10_080),
    ("seven_day_sonnet", "7d_sonnet", "7d sonnet", 10_080),
];

#[derive(Debug, PartialEq, Eq)]
struct Credentials {
    token: String,
    expires_at_ms: Option<i64>,
    subscription: Option<String>,
    tier: Option<String>,
}

/// `{"claudeAiOauth": {accessToken, expiresAt, subscriptionType, rateLimitTier}}`.
fn parse_credentials(bytes: &[u8]) -> Option<Credentials> {
    let root: Value = serde_json::from_slice(bytes).ok()?;
    let oauth = root.get("claudeAiOauth")?;
    Some(Credentials {
        token: text(oauth.get("accessToken"))?.to_owned(),
        expires_at_ms: oauth.get("expiresAt").and_then(Value::as_i64),
        subscription: text(oauth.get("subscriptionType")).map(str::to_owned),
        tier: text(oauth.get("rateLimitTier")).map(str::to_owned),
    })
}

fn read_credentials(sources: &Sources) -> Option<Credentials> {
    // No Claude Code directory, no Claude Code: never touch the Keychain of
    // a home that has not run it.
    if !sources.claude_dir.is_dir() {
        return None;
    }
    if sources.keychain
        && let Some(creds) = keychain_secret().as_deref().and_then(parse_credentials)
    {
        return Some(creds);
    }
    let bytes = std::fs::read(sources.claude_dir.join(".credentials.json")).ok()?;
    parse_credentials(&bytes)
}

/// The Keychain item's secret, through `security` (which Claude Code's
/// item already trusts), bounded so a locked keychain cannot hang a hook.
fn keychain_secret() -> Option<Vec<u8>> {
    let mut child = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait().ok()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    status.success().then_some(out)
}

/// `~/.claude.json` `oauthAccount.accountUuid`.
fn account_id(sources: &Sources) -> Option<String> {
    let bytes = std::fs::read(&sources.claude_account).ok()?;
    let root: Value = serde_json::from_slice(&bytes).ok()?;
    text(root.pointer("/oauthAccount/accountUuid")).map(str::to_owned)
}

/// `default_claude_max_20x` reads as `Max 20x`; otherwise the plan name.
fn label(subscription: Option<&str>, tier: Option<&str>) -> Option<String> {
    let tier = tier.unwrap_or_default();
    for multiple in ["20x", "5x"] {
        if tier.ends_with(&format!("max_{multiple}")) {
            return Some(format!("Max {multiple}"));
        }
    }
    let sub = subscription?;
    let mut chars = sub.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// `{five_hour: {utilization, resets_at}, seven_day: …}`; null windows are
/// absent.
fn parse_usage(body: &[u8]) -> Result<Vec<Window>, Failure> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|_| Failure::Failed("Claude usage reply was not JSON".to_owned()))?;
    let windows: Vec<Window> = WINDOWS
        .iter()
        .filter_map(|(field, id, label, minutes)| {
            let entry = root.get(*field)?;
            Some(Window {
                id: (*id).to_owned(),
                label: (*label).to_owned(),
                minutes: *minutes,
                used_percent: number(entry.get("utilization"))?,
                resets_at_ms: text(entry.get("resets_at")).and_then(iso_ms),
            })
        })
        .collect();
    if windows.is_empty() {
        return Err(Failure::Failed(
            "Claude usage reply had no windows".to_owned(),
        ));
    }
    Ok(windows)
}

pub(crate) fn collect(
    sources: &Sources,
    source: &mut SourceState,
    request: Request<'_>,
) -> Option<Plan> {
    let creds = read_credentials(sources)?;
    let base = Plan {
        plan: creds.subscription.clone(),
        label: label(creds.subscription.as_deref(), creds.tier.as_deref()),
        account: account_id(sources).as_deref().and_then(fingerprint),
        ..identity("claude", "claude-code", "oauth")
    };
    if creds.expires_at_ms.is_some_and(|at| at <= request.now_ms) {
        return Some(with_last_good(
            source,
            base,
            Status::Unavailable,
            Some("sign-in expired; run claude to refresh it".to_owned()),
        ));
    }
    Some(polled(source, base, request, || {
        let reply = request
            .fetch
            .get(
                USAGE_URL,
                &[
                    ("Authorization", format!("Bearer {}", creds.token)),
                    ("anthropic-beta", BETA.to_owned()),
                    ("User-Agent", USER_AGENT.to_owned()),
                    ("Accept", "application/json".to_owned()),
                ],
            )
            .map_err(Failure::Failed)?;
        if reply.status != 200 {
            return Err(Failure::from_status(reply.status, "Anthropic"));
        }
        parse_usage(&reply.body)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Script;

    const CREDS: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-test","refreshToken":"rt",
        "expiresAt":1783600000000,"scopes":["user:inference"],
        "subscriptionType":"max","rateLimitTier":"default_claude_max_20x"}}"#;

    /// A trimmed real reply: codename windows and `extra_usage` are ignored.
    const USAGE: &str = r#"{"five_hour":{"utilization":39.0,"resets_at":"2026-10-20T03:49:59.556817+00:00","limit_dollars":null},
        "seven_day":{"utilization":20.0,"resets_at":"2026-10-26T00:59:59.556841+00:00"},
        "seven_day_oauth_apps":null,"seven_day_opus":null,
        "seven_day_sonnet":{"utilization":1.5,"resets_at":"2026-10-26T00:59:59+00:00"},
        "seven_day_omelette":null,"extra_usage":{"is_enabled":false,"utilization":0.0}}"#;

    fn sources(dir: &std::path::Path) -> Sources {
        Sources {
            claude_dir: dir.join("claude"),
            claude_account: dir.join("claude.json"),
            keychain: false,
            codex_home: dir.join("codex"),
            grok_home: dir.join("grok"),
            grok_proxy: String::new(),
        }
    }

    fn request(fetch: &Script, now_ms: i64) -> Request<'_> {
        Request {
            host: "h",
            now_ms,
            eager: false,
            fetch,
        }
    }

    #[test]
    fn credentials_parse_and_tiers_label() {
        let creds = parse_credentials(CREDS.as_bytes()).unwrap();
        assert_eq!(creds.token, "sk-ant-test");
        assert_eq!(creds.expires_at_ms, Some(1_783_600_000_000));
        assert_eq!(creds.subscription.as_deref(), Some("max"));
        assert!(parse_credentials(br#"{"claudeAiOauth":{}}"#).is_none());
        assert_eq!(
            label(Some("max"), Some("default_claude_max_20x")).as_deref(),
            Some("Max 20x")
        );
        assert_eq!(
            label(Some("max"), Some("default_claude_max_5x")).as_deref(),
            Some("Max 5x")
        );
        assert_eq!(label(Some("pro"), None).as_deref(), Some("Pro"));
        assert_eq!(label(None, None), None);
    }

    #[test]
    fn usage_maps_known_windows_and_skips_nulls() {
        let windows = parse_usage(USAGE.as_bytes()).unwrap();
        let ids: Vec<&str> = windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["5h", "7d", "7d_sonnet"]);
        assert_eq!(windows[0].used_percent, 39.0);
        assert_eq!(windows[0].minutes, 300);
        assert_eq!(
            windows[0].resets_at_ms,
            iso_ms("2026-10-20T03:49:59.556817Z")
        );
        assert!(matches!(parse_usage(b"{}"), Err(Failure::Failed(_))));
        assert!(matches!(parse_usage(b"<html>"), Err(Failure::Failed(_))));
    }

    #[test]
    fn collect_sends_the_token_only_to_anthropic_and_fingerprints_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let sources = sources(dir.path());
        std::fs::create_dir_all(&sources.claude_dir).unwrap();
        std::fs::write(sources.claude_dir.join(".credentials.json"), CREDS).unwrap();
        std::fs::write(
            &sources.claude_account,
            r#"{"oauthAccount":{"accountUuid":"abc","emailAddress":"x@y"}}"#,
        )
        .unwrap();
        let script = Script::default().reply(USAGE_URL, 200, USAGE);
        let mut state = SourceState::default();
        let plan = collect(&sources, &mut state, request(&script, 1_000)).unwrap();
        assert_eq!(script.requests(), [USAGE_URL]);
        assert_eq!(
            script.header("Authorization").as_deref(),
            Some("Bearer sk-ant-test")
        );
        assert_eq!(script.header("anthropic-beta").as_deref(), Some(BETA));
        assert_eq!(plan.status, Status::Ok);
        assert_eq!(plan.label.as_deref(), Some("Max 20x"));
        assert_eq!(plan.account.as_deref(), Some("ba7816bf8f01"));
        assert_eq!(plan.fetched_at_ms, Some(1_000));
        let json = serde_json::to_string(&plan).unwrap();
        assert!(!json.contains("sk-ant") && !json.contains("x@y"));
    }

    #[test]
    fn an_expired_token_is_unavailable_without_a_request() {
        let dir = tempfile::tempdir().unwrap();
        let sources = sources(dir.path());
        std::fs::create_dir_all(&sources.claude_dir).unwrap();
        std::fs::write(sources.claude_dir.join(".credentials.json"), CREDS).unwrap();
        let script = Script::default();
        let mut state = SourceState::default();
        let plan = collect(&sources, &mut state, request(&script, 1_783_600_000_000)).unwrap();
        assert_eq!(plan.status, Status::Unavailable);
        assert!(plan.error.unwrap().contains("expired"));
        assert!(script.requests().is_empty());
    }

    #[test]
    fn a_429_backs_off_and_no_credentials_means_no_plan() {
        let dir = tempfile::tempdir().unwrap();
        let sources = sources(dir.path());
        let script = Script::default();
        let mut state = SourceState::default();
        assert!(collect(&sources, &mut state, request(&script, 0)).is_none());

        std::fs::create_dir_all(&sources.claude_dir).unwrap();
        std::fs::write(sources.claude_dir.join(".credentials.json"), CREDS).unwrap();
        let script = Script::default().reply(USAGE_URL, 429, "{}");
        let plan = collect(&sources, &mut state, request(&script, 0)).unwrap();
        assert_eq!(
            plan.status,
            Status::Error,
            "no earlier reading to fall back on"
        );
        assert_eq!(plan.error.as_deref(), Some("rate limited, retry in 3m"));
        assert_eq!(state.retry_at_ms, 180_000);
    }
}
