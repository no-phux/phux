//! Grok Build: the credits allowance its `/usage` view shows, from the CLI
//! proxy's `GET /billing?format=credits`, authorized by the OIDC session in
//! `~/.grok/auth.json`. The plan comes from that token's `tier` claim. The
//! session is read, never refreshed.

use serde_json::Value;

use crate::model::{Plan, Status, Window};
use crate::state::SourceState;
use crate::util::{fingerprint, iso_ms, jwt_claims, number, text};
use crate::{Failure, Request, Sources, identity, polled, with_last_good};

/// State key.
pub(crate) const SOURCE: &str = "grok";
/// Grok Build's default CLI proxy.
pub(crate) const DEFAULT_PROXY: &str = "https://cli-chat-proxy.grok.com/v1";
/// The value Grok Build sends in `X-XAI-Token-Auth`.
const TOKEN_AUTH: &str = "xai-grok-cli";
const CLIENT_VERSION: &str = "0.1.0";

#[derive(Debug, PartialEq, Eq)]
struct Session {
    key: String,
    user_id: Option<String>,
    expires_at_ms: Option<i64>,
}

/// `auth.json` maps `issuer::client` to a session; take the OIDC one that
/// expires last.
fn parse_auth(bytes: &[u8]) -> Option<Session> {
    let root: Value = serde_json::from_slice(bytes).ok()?;
    root.as_object()?
        .values()
        .filter(|entry| text(entry.get("auth_mode")).is_none_or(|mode| mode == "oidc"))
        .filter_map(|entry| {
            Some(Session {
                key: text(entry.get("key"))?.to_owned(),
                user_id: text(entry.get("user_id")).map(str::to_owned),
                expires_at_ms: text(entry.get("expires_at")).and_then(iso_ms),
            })
        })
        .max_by_key(|session| session.expires_at_ms.unwrap_or(i64::MIN))
}

/// The JWT `tier` claim, numbered as Grok Build numbers it.
fn tier(key: &str) -> Option<(&'static str, &'static str)> {
    let tier = jwt_claims(key)?.get("tier")?.as_u64()?;
    Some(match tier {
        0 => ("free", "Free"),
        1 => ("supergrok", "SuperGrok"),
        2 => ("x_basic", "X Basic"),
        3 => ("x_premium", "X Premium"),
        4 => ("x_premium_plus", "X Premium+"),
        5 => ("supergrok_heavy", "SuperGrok Heavy"),
        6 => ("supergrok_lite", "SuperGrok Lite"),
        7 => ("supergrok_plus", "SuperGrok Plus"),
        _ => return None,
    })
}

/// `{config: {creditUsagePercent, currentPeriod: {type, start, end}}}`,
/// falling back to the deprecated `used`/`monthlyLimit` and
/// `billingPeriodStart`/`End`.
fn parse_billing(body: &[u8]) -> Result<Vec<Window>, Failure> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|_| Failure::Failed("xAI billing reply was not JSON".to_owned()))?;
    let config = root
        .get("config")
        .filter(|c| c.is_object())
        .ok_or_else(|| Failure::Failed("xAI billing reply had no allowance".to_owned()))?;
    let cents = |key: &str| number(config.get(key).and_then(|c| c.get("val")));
    let used = number(config.get("creditUsagePercent")).or_else(|| {
        let limit = cents("monthlyLimit").filter(|limit| *limit > 0.0)?;
        Some(cents("used").unwrap_or(0.0) / limit * 100.0)
    });
    let Some(used) = used else {
        return Err(Failure::Failed("xAI billing reply had no usage".to_owned()));
    };
    let period = config.get("currentPeriod");
    let bound = |key: &str, legacy: &str| {
        text(period.and_then(|p| p.get(key)))
            .or_else(|| text(config.get(legacy)))
            .and_then(iso_ms)
    };
    let start = bound("start", "billingPeriodStart");
    let end = bound("end", "billingPeriodEnd");
    let kind = text(period.and_then(|p| p.get("type"))).unwrap_or_default();
    let minutes = match (start, end) {
        (Some(start), Some(end)) if end > start => {
            u32::try_from((end - start + 30_000) / 60_000).unwrap_or(0)
        }
        _ if kind.ends_with("WEEKLY") => 10_080,
        _ => 43_200,
    };
    let mut window = Window::by_length(minutes, used, end);
    if kind.ends_with("MONTHLY") {
        "month".clone_into(&mut window.id);
        "month".clone_into(&mut window.label);
    }
    Ok(vec![window])
}

pub(crate) fn collect(
    sources: &Sources,
    source: &mut SourceState,
    request: Request<'_>,
) -> Option<Plan> {
    let bytes = std::fs::read(sources.grok_home.join("auth.json")).ok()?;
    let session = parse_auth(&bytes)?;
    let tier = tier(&session.key);
    let base = Plan {
        plan: tier.map(|(id, _)| id.to_owned()),
        label: tier.map(|(_, label)| label.to_owned()),
        account: session.user_id.as_deref().and_then(fingerprint),
        ..identity("grok", "grok-build", "billing")
    };
    if session.expires_at_ms.is_some_and(|at| at <= request.now_ms) {
        return Some(with_last_good(
            source,
            base,
            Status::Unavailable,
            Some("sign-in expired; run grok to refresh it".to_owned()),
        ));
    }
    let url = format!(
        "{}/billing?format=credits",
        sources.grok_proxy.trim_end_matches('/')
    );
    Some(polled(source, base, request, || {
        let mut headers = vec![
            ("Authorization", format!("Bearer {}", session.key)),
            ("X-XAI-Token-Auth", TOKEN_AUTH.to_owned()),
            ("x-grok-client-version", CLIENT_VERSION.to_owned()),
            ("Accept", "application/json".to_owned()),
        ];
        if let Some(user) = &session.user_id {
            headers.push(("x-userid", user.clone()));
        }
        let reply = request.fetch.get(&url, &headers).map_err(Failure::Failed)?;
        if reply.status != 200 {
            return Err(Failure::from_status(reply.status, "xAI"));
        }
        parse_billing(&reply.body)
    }))
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;
    use crate::Script;

    /// A real reply's shape.
    const CREDITS: &str = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-10-13T02:39:37.078+00:00","end":"2026-10-20T02:39:37.078+00:00"},"creditUsagePercent":100.0,"productUsage":[{"product":"Cursor","usagePercent":51.0}],"isUnifiedBillingUser":true,"prepaidBalance":{"val":0}}}"#;

    fn key(tier: u8) -> String {
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.sig",
            enc.encode(b"{}"),
            enc.encode(format!(r#"{{"tier":{tier}}}"#))
        )
    }

    fn auth(expires: &str) -> String {
        format!(
            r#"{{"https://auth.x.ai::client":{{"key":"{}","auth_mode":"oidc","user_id":"u-1","email":"x@y","refresh_token":"r","expires_at":"{expires}"}}}}"#,
            key(5)
        )
    }

    fn sources(dir: &std::path::Path) -> Sources {
        Sources {
            claude_dir: dir.join("none"),
            claude_account: dir.join("none.json"),
            keychain: false,
            codex_home: dir.join("none"),
            grok_home: dir.join("grok"),
            grok_proxy: "https://proxy.test/v1/".to_owned(),
        }
    }

    #[test]
    fn credits_map_to_one_weekly_window() {
        let windows = parse_billing(CREDITS.as_bytes()).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id, "7d");
        assert_eq!(windows[0].minutes, 10_080);
        assert_eq!(windows[0].used_percent, 100.0);
        assert_eq!(windows[0].resets_at_ms, iso_ms("2026-10-20T02:39:37.078Z"));
    }

    #[test]
    fn the_legacy_monthly_shape_still_reads() {
        let legacy = r#"{"config":{"monthlyLimit":{"val":2000},"used":{"val":500},"billingPeriodStart":"2025-04-01T00:00:00Z","billingPeriodEnd":"2025-05-01T00:00:00Z"}}"#;
        let windows = parse_billing(legacy.as_bytes()).unwrap();
        assert_eq!(windows[0].used_percent, 25.0);
        assert_eq!(windows[0].id, "30d");
        let monthly = r#"{"config":{"creditUsagePercent":3,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY"}}}"#;
        assert_eq!(parse_billing(monthly.as_bytes()).unwrap()[0].id, "month");
        assert!(parse_billing(br#"{"config":null}"#).is_err());
        assert!(parse_billing(br#"{"config":{}}"#).is_err());
    }

    #[test]
    fn tiers_and_sessions_parse() {
        assert_eq!(tier(&key(5)), Some(("supergrok_heavy", "SuperGrok Heavy")));
        assert_eq!(tier(&key(0)), Some(("free", "Free")));
        assert_eq!(tier("opaque"), None);
        let session = parse_auth(auth("2026-10-20T02:09:54.324114Z").as_bytes()).unwrap();
        assert_eq!(session.user_id.as_deref(), Some("u-1"));
        assert_eq!(session.expires_at_ms, iso_ms("2026-10-20T02:09:54.324Z"));
        assert!(parse_auth(br#"{"x":{"auth_mode":"api_key","key":"k"}}"#).is_none());
    }

    #[test]
    fn collect_reads_billing_with_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let sources = sources(dir.path());
        std::fs::create_dir_all(&sources.grok_home).unwrap();
        std::fs::write(
            sources.grok_home.join("auth.json"),
            auth("2100-01-01T00:00:00Z"),
        )
        .unwrap();
        let url = "https://proxy.test/v1/billing?format=credits";
        let script = Script::default().reply(url, 200, CREDITS);
        let plan = collect(
            &sources,
            &mut SourceState::default(),
            Request {
                host: "h",
                now_ms: 0,
                eager: false,
                fetch: &script,
            },
        )
        .unwrap();
        assert_eq!(script.requests(), [url]);
        assert_eq!(script.header("x-userid").as_deref(), Some("u-1"));
        assert_eq!(plan.label.as_deref(), Some("SuperGrok Heavy"));
        assert_eq!(plan.account, fingerprint("u-1"));
        assert_eq!(plan.status, Status::Ok);

        std::fs::write(
            sources.grok_home.join("auth.json"),
            auth("2000-01-01T00:00:00Z"),
        )
        .unwrap();
        let script = Script::default();
        let plan = collect(
            &sources,
            &mut SourceState::default(),
            Request {
                host: "h",
                now_ms: 1_000_000_000_000,
                eager: false,
                fetch: &script,
            },
        )
        .unwrap();
        assert_eq!(plan.status, Status::Unavailable);
        assert!(script.requests().is_empty());
    }
}
