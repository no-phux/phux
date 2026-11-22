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
#[derive(Default, PartialEq)]
struct Auth {
    plan: Option<String>,
    account_id: Option<String>,
    access_token: Option<String>,
    expires_at_ms: Option<i64>,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("plan", &self.plan)
            .field("account_id", &self.account_id)
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
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
    let minutes = u32::try_from(window.get("window_minutes")?.as_u64()?)
        .ok()
        .filter(|m| *m > 0)?;
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
            let minutes = u32::try_from(window.get("limit_window_seconds")?.as_u64()? / 60)
                .ok()
                .filter(|m| *m > 0)?;
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
    let account = auth.account_id.clone();
    let live = polled(
        wham,
        Plan {
            source: "wham".to_owned(),
            ..base.clone()
        },
        request,
        || {
            let mut headers = vec![
                ("Authorization", format!("Bearer {token}")),
                ("User-Agent", USER_AGENT.to_owned()),
                ("Accept", "application/json".to_owned()),
            ];
            if let Some(account) = account {
                headers.push(("ChatGPT-Account-Id", account));
            }
            let reply = request
                .fetch
                .get(WHAM_URL, &headers)
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
mod tests;
