//! Codex: every `token_count` event in a rollout ledger
//! (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl`) carries the account's
//! current `rate_limits`, so a recent session answers with no network at
//! all. When the newest reading is older than [`ROLLOUT_FRESH_MS`], the
//! `ChatGPT` usage endpoint (`/backend-api/wham/usage`) is asked instead,
//! with the access token Codex stores in `auth.json` (never refreshed).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::model::{Plan, Status, Window};
use crate::state::{STALE_AFTER_MS, SourceState};
use crate::util::{fingerprint, iso_ms, jwt_claims, number, seconds, text};
use crate::{Failure, Request, Sources, identity, polled};

/// State key for the live endpoint.
pub(crate) const WHAM_SOURCE: &str = "codex-wham";
const WHAM_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const USER_AGENT: &str = "codex_cli_rs/0.142.0";
/// A rollout reading this recent is used as is.
const ROLLOUT_FRESH_MS: i64 = 10 * 60_000;
/// Day directories and files examined per pass, newest first.
const MAX_DAYS: usize = 7;
const MAX_FILES: usize = 8;
/// Only the tail of a ledger is read; the newest event is at its end.
const TAIL_BYTES: u64 = 1024 * 1024;

/// One `rate_limits` observation.
#[derive(Debug, Clone, PartialEq)]
struct Reading {
    at_ms: i64,
    plan: Option<String>,
    windows: Vec<Window>,
}

/// What `auth.json` says about the account.
#[derive(Debug, Default, PartialEq)]
struct Auth {
    plan: Option<String>,
    account_id: Option<String>,
    access_token: Option<String>,
    expires_at_ms: Option<i64>,
}

fn parse_auth(bytes: &[u8]) -> Option<Auth> {
    let root: Value = serde_json::from_slice(bytes).ok()?;
    let tokens = root.get("tokens")?;
    let claims = text(tokens.get("id_token")).and_then(jwt_claims);
    let profile = claims
        .as_ref()
        .and_then(|c| c.get("https://api.openai.com/auth"));
    let access_token = text(tokens.get("access_token")).map(str::to_owned);
    let expires_at_ms = access_token
        .as_deref()
        .and_then(jwt_claims)
        .and_then(|c| seconds(c.get("exp")))
        .map(|s| s * 1000);
    Some(Auth {
        plan: profile
            .and_then(|p| text(p.get("chatgpt_plan_type")))
            .map(str::to_owned),
        account_id: text(tokens.get("account_id"))
            .or_else(|| profile.and_then(|p| text(p.get("chatgpt_account_id"))))
            .map(str::to_owned),
        access_token,
        expires_at_ms,
    })
}

/// A `rate_limits` window: `{used_percent, window_minutes, resets_at}`
/// (older builds send `resets_in_seconds`).
fn rollout_window(window: &Value, at_ms: i64) -> Option<Window> {
    let minutes = u32::try_from(window.get("window_minutes")?.as_u64()?).ok()?;
    let resets_at_ms = seconds(window.get("resets_at"))
        .map(|s| s * 1000)
        .or_else(|| seconds(window.get("resets_in_seconds")).map(|s| at_ms + s * 1000));
    Some(Window::by_length(
        minutes,
        number(window.get("used_percent"))?,
        resets_at_ms,
    ))
}

/// One ledger line, when it is a `token_count` event carrying limits.
fn parse_line(line: &str) -> Option<Reading> {
    if !line.contains("token_count") {
        return None;
    }
    let root: Value = serde_json::from_str(line).ok()?;
    if text(root.get("type")) != Some("event_msg") {
        return None;
    }
    let payload = root.get("payload")?;
    if text(payload.get("type")) != Some("token_count") {
        return None;
    }
    let limits = payload.get("rate_limits")?;
    let at_ms = text(root.get("timestamp")).and_then(iso_ms)?;
    let mut windows: Vec<Window> = ["primary", "secondary"]
        .iter()
        .filter_map(|key| limits.get(*key).and_then(|w| rollout_window(w, at_ms)))
        .collect();
    if windows.is_empty() {
        return None;
    }
    windows.sort_by_key(|w| w.minutes);
    Some(Reading {
        at_ms,
        plan: text(limits.get("plan_type")).map(str::to_owned),
        windows,
    })
}

/// The newest reading in the tail of one ledger.
fn last_reading(path: &Path) -> Option<Reading> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    // A tail that starts mid-line drops that fragment.
    let lines = tail.lines().skip(usize::from(start > 0));
    lines.filter_map(parse_line).max_by_key(|r| r.at_ms)
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs.reverse();
    dirs
}

/// Ledgers in the newest day directories, most recently written first.
fn recent_rollouts(codex_home: &Path) -> Vec<PathBuf> {
    let mut days = Vec::new();
    'walk: for year in sorted_dirs(&codex_home.join("sessions")) {
        for month in sorted_dirs(&year) {
            for day in sorted_dirs(&month) {
                days.push(day);
                if days.len() == MAX_DAYS {
                    break 'walk;
                }
            }
        }
    }
    let mut files: Vec<(i64, PathBuf)> = days
        .iter()
        .flat_map(|day| std::fs::read_dir(day).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("rollout-")
                    && Path::new(n)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
            })
        })
        .map(|path| (modified_ms(&path), path))
        .collect();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    files
        .into_iter()
        .take(MAX_FILES)
        .map(|(_, path)| path)
        .collect()
}

fn modified_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// The newest reading across the recent ledgers.
fn newest_reading(codex_home: &Path) -> Option<Reading> {
    let mut best: Option<Reading> = None;
    for path in recent_rollouts(codex_home) {
        // Files are newest-written first: one last written before the best
        // reading cannot hold a newer one.
        if best.as_ref().is_some_and(|b| modified_ms(&path) < b.at_ms) {
            break;
        }
        if let Some(reading) = last_reading(&path)
            && best.as_ref().is_none_or(|b| reading.at_ms > b.at_ms)
        {
            best = Some(reading);
        }
    }
    best
}

/// The live endpoint's reply:
/// `{plan_type, rate_limit: {primary_window: {used_percent, limit_window_seconds, reset_at}, secondary_window}}`.
fn parse_wham(body: &[u8]) -> Result<(Option<String>, Vec<Window>), Failure> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|_| Failure::Failed("ChatGPT usage reply was not JSON".to_owned()))?;
    let limits = root.get("rate_limit");
    let mut windows: Vec<Window> = ["primary_window", "secondary_window"]
        .iter()
        .filter_map(|key| {
            let window = limits?.get(*key)?;
            let minutes = u32::try_from(window.get("limit_window_seconds")?.as_u64()? / 60).ok()?;
            Some(Window::by_length(
                minutes,
                number(window.get("used_percent"))?,
                seconds(window.get("reset_at")).map(|s| s * 1000),
            ))
        })
        .collect();
    if windows.is_empty() {
        return Err(Failure::Failed(
            "ChatGPT usage reply had no windows".to_owned(),
        ));
    }
    windows.sort_by_key(|w| w.minutes);
    Ok((text(root.get("plan_type")).map(str::to_owned), windows))
}

fn label(plan: &str) -> String {
    let mut chars = plan.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A rollout reading as a plan: `ok` while recent, `stale` once old.
fn from_rollout(base: Plan, reading: Reading, now: i64, error: Option<String>) -> Plan {
    let stale = now - reading.at_ms > STALE_AFTER_MS;
    Plan {
        source: "rollout".to_owned(),
        status: if stale { Status::Stale } else { Status::Ok },
        error: if stale { error } else { None },
        fetched_at_ms: Some(reading.at_ms),
        windows: reading.windows,
        ..base
    }
}

pub(crate) fn collect(
    sources: &Sources,
    wham: &mut SourceState,
    request: Request<'_>,
) -> Option<Plan> {
    let auth = std::fs::read(sources.codex_home.join("auth.json"))
        .ok()
        .and_then(|bytes| parse_auth(&bytes));
    let reading = newest_reading(&sources.codex_home);
    if auth.is_none() && reading.is_none() {
        return None;
    }
    let auth = auth.unwrap_or_default();
    let plan_id = reading
        .as_ref()
        .and_then(|r| r.plan.clone())
        .or_else(|| auth.plan.clone());
    let base = Plan {
        label: plan_id.as_deref().map(label),
        plan: plan_id,
        account: auth.account_id.as_deref().and_then(fingerprint),
        ..identity("codex", "codex", "rollout")
    };
    let now = request.now_ms;
    if let Some(reading) = reading
        .clone()
        .filter(|r| now - r.at_ms <= ROLLOUT_FRESH_MS)
    {
        return Some(from_rollout(base, reading, now, None));
    }
    let token = auth
        .access_token
        .as_deref()
        .filter(|_| auth.expires_at_ms.is_none_or(|at| at > now));
    let Some(token) = token else {
        let why = if auth.access_token.is_some() {
            "sign-in expired; run codex to refresh it"
        } else {
            "no recent Codex session"
        };
        return Some(reading.map_or_else(
            || Plan {
                status: Status::Unavailable,
                error: Some(why.to_owned()),
                ..base.clone()
            },
            |reading| from_rollout(base.clone(), reading, now, Some(why.to_owned())),
        ));
    };
    let account = auth.account_id.clone().unwrap_or_default();
    let live = polled(
        wham,
        Plan {
            source: "wham".to_owned(),
            ..base.clone()
        },
        request,
        || {
            let reply = request
                .fetch
                .get(
                    WHAM_URL,
                    &[
                        ("Authorization", format!("Bearer {token}")),
                        ("ChatGPT-Account-Id", account),
                        ("User-Agent", USER_AGENT.to_owned()),
                        ("Accept", "application/json".to_owned()),
                    ],
                )
                .map_err(Failure::Failed)?;
            if reply.status != 200 {
                return Err(Failure::from_status(reply.status, "ChatGPT"));
            }
            parse_wham(&reply.body).map(|(_, windows)| windows)
        },
    );
    match reading {
        Some(reading)
            if live.status != Status::Ok
                && live.fetched_at_ms.is_none_or(|at| at < reading.at_ms) =>
        {
            Some(from_rollout(base, reading, now, live.error))
        }
        _ => Some(live),
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;
    use crate::Script;

    /// A current codex line: only a weekly `primary`, `secondary` null.
    const WEEKLY: &str = r#"{"timestamp":"2026-10-19T23:49:51.736Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1}},"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":43.0,"window_minutes":10080,"resets_at":1792874847},"secondary":null,"credits":{"has_credits":true},"plan_type":"pro","rate_limit_reached_type":null}}}"#;
    /// An older line with both windows.
    const BOTH: &str = r#"{"timestamp":"2025-10-10T08:00:00Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":42.5,"window_minutes":300,"resets_at":1760016000},"secondary":{"used_percent":7.75,"window_minutes":10080,"resets_at":1760400000},"plan_type":"plus"}}}"#;

    fn jwt(claims: &str) -> String {
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.sig",
            enc.encode(b"{}"),
            enc.encode(claims.as_bytes())
        )
    }

    fn auth_json(exp_s: i64) -> String {
        let id = jwt(
            r#"{"https://api.openai.com/auth":{"chatgpt_plan_type":"pro","chatgpt_account_id":"acct-1"},"email":"x@y"}"#,
        );
        let access = jwt(&format!(r#"{{"exp":{exp_s}}}"#));
        format!(
            r#"{{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{{"id_token":"{id}","access_token":"{access}","refresh_token":"r","account_id":"acct-1"}}}}"#
        )
    }

    fn setup(lines: &[&str], auth: Option<String>) -> (tempfile::TempDir, Sources) {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("codex");
        let day = codex.join("sessions/2026/10/19");
        std::fs::create_dir_all(&day).unwrap();
        let mut body = String::from("{\"type\":\"session_meta\",\"payload\":{}}\n");
        for line in lines {
            body.push_str(line);
            body.push('\n');
        }
        body.push_str(
            "{\"type\":\"response_item\",\"payload\":{\"text\":\"token_count mention\"}}\n",
        );
        std::fs::write(day.join("rollout-2026-10-19T19-35-41-x.jsonl"), body).unwrap();
        if let Some(auth) = auth {
            std::fs::write(codex.join("auth.json"), auth).unwrap();
        }
        let sources = Sources {
            claude_dir: dir.path().join("none"),
            claude_account: dir.path().join("none.json"),
            keychain: false,
            codex_home: codex,
            grok_home: dir.path().join("none"),
            grok_proxy: String::new(),
        };
        (dir, sources)
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
    fn lines_map_windows_by_length_and_skip_noise() {
        let weekly = parse_line(WEEKLY).unwrap();
        assert_eq!(weekly.plan.as_deref(), Some("pro"));
        assert_eq!(weekly.windows.len(), 1);
        assert_eq!(weekly.windows[0].id, "7d");
        assert_eq!(weekly.windows[0].resets_at_ms, Some(1_792_874_847_000));
        let both = parse_line(BOTH).unwrap();
        let ids: Vec<&str> = both.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["5h", "7d"]);
        assert_eq!(both.windows[1].used_percent, 7.75);
        let legacy = r#"{"timestamp":"2000-01-01T00:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":1.0,"window_minutes":300,"resets_in_seconds":60}}}}"#;
        assert_eq!(
            parse_line(legacy).unwrap().windows[0].resets_at_ms,
            Some(946_684_860_000)
        );
        assert!(parse_line(r#"{"type":"response_item","payload":{"t":"token_count"}}"#).is_none());
        assert!(parse_line(r#"{"timestamp":"2000-01-01T00:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":null}}"#).is_none());
        assert!(parse_line("token_count {broken").is_none());
    }

    #[test]
    fn auth_yields_plan_account_and_token_expiry() {
        let auth = parse_auth(auth_json(2_000).as_bytes()).unwrap();
        assert_eq!(auth.plan.as_deref(), Some("pro"));
        assert_eq!(auth.account_id.as_deref(), Some("acct-1"));
        assert_eq!(auth.expires_at_ms, Some(2_000_000));
        assert!(parse_auth(br#"{"OPENAI_API_KEY":"sk"}"#).is_none());
    }

    #[test]
    fn the_newest_line_wins_and_a_fresh_one_needs_no_network() {
        let (_dir, sources) = setup(&[BOTH, WEEKLY], Some(auth_json(i64::MAX / 2000)));
        let at = iso_ms("2026-10-19T23:49:51.736Z").unwrap();
        let script = Script::default();
        let mut wham = SourceState::default();
        let plan = collect(&sources, &mut wham, request(&script, at + 60_000)).unwrap();
        assert!(script.requests().is_empty());
        assert_eq!(plan.source, "rollout");
        assert_eq!(plan.status, Status::Ok);
        assert_eq!(plan.fetched_at_ms, Some(at));
        assert_eq!(plan.label.as_deref(), Some("Pro"));
        assert_eq!(plan.account, fingerprint("acct-1"));
        assert_eq!(plan.windows[0].used_percent, 43.0);
    }

    #[test]
    fn an_old_reading_asks_the_live_endpoint() {
        let (_dir, sources) = setup(&[BOTH], Some(auth_json(i64::MAX / 2000)));
        let wham_body = r#"{"plan_type":"pro","rate_limit":{"allowed":true,"primary_window":{"used_percent":43,"limit_window_seconds":604800,"reset_after_seconds":415208,"reset_at":1792874847},"secondary_window":null},"credits":{"balance":"1"}}"#;
        let script = Script::default().reply(WHAM_URL, 200, wham_body);
        let mut wham = SourceState::default();
        let now = iso_ms("2026-10-20T00:00:00Z").unwrap();
        let plan = collect(&sources, &mut wham, request(&script, now)).unwrap();
        assert_eq!(
            script.header("ChatGPT-Account-Id").as_deref(),
            Some("acct-1")
        );
        assert_eq!(plan.source, "wham");
        assert_eq!(plan.status, Status::Ok);
        assert_eq!(plan.windows[0].id, "7d");
        assert_eq!(plan.windows[0].resets_at_ms, Some(1_792_874_847_000));

        // A failing endpoint falls back to the old ledger reading, as stale.
        let (_dir, sources) = setup(&[BOTH], Some(auth_json(i64::MAX / 2000)));
        let script = Script::default().reply(WHAM_URL, 500, "");
        let plan = collect(&sources, &mut SourceState::default(), request(&script, now)).unwrap();
        assert_eq!(plan.source, "rollout");
        assert_eq!(plan.status, Status::Stale);
        assert_eq!(plan.error.as_deref(), Some("ChatGPT answered HTTP 500"));
    }

    #[test]
    fn an_expired_token_never_reaches_the_network() {
        let (_dir, sources) = setup(&[], Some(auth_json(1)));
        let script = Script::default();
        let plan = collect(
            &sources,
            &mut SourceState::default(),
            request(&script, 10_000),
        )
        .unwrap();
        assert_eq!(plan.status, Status::Unavailable);
        assert!(plan.error.unwrap().contains("expired"));
        assert!(script.requests().is_empty());
    }

    #[test]
    fn only_the_tail_of_a_large_ledger_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-big.jsonl");
        let filler = format!(
            "{{\"type\":\"response_item\",\"pad\":\"{}\"}}\n",
            "x".repeat(4096)
        );
        let mut body = String::from(BOTH);
        body.push('\n');
        while (body.len() as u64) < TAIL_BYTES + 8192 {
            body.push_str(&filler);
        }
        body.push_str(WEEKLY);
        body.push('\n');
        std::fs::write(&path, body).unwrap();
        let reading = last_reading(&path).unwrap();
        assert_eq!(reading.plan.as_deref(), Some("pro"));
    }

    #[test]
    fn wham_rejects_replies_without_windows() {
        assert!(parse_wham(br#"{"rate_limit":{"primary_window":null}}"#).is_err());
        assert!(parse_wham(b"nope").is_err());
    }
}
