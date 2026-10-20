//! Codex ledger, auth and live-endpoint parsing against fixtures.

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
    body.push_str("{\"type\":\"response_item\",\"payload\":{\"text\":\"token_count mention\"}}\n");
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
    let zero = br#"{"rate_limit":{"primary_window":{"used_percent":1,"limit_window_seconds":0}}}"#;
    assert!(parse_wham(zero).is_err(), "a zero-length window is dropped");
    let zero = r#"{"timestamp":"2000-01-01T00:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":1.0,"window_minutes":0}}}}"#;
    assert!(parse_line(zero).is_none());
    let auth = parse_auth(auth_json(2_000).as_bytes()).unwrap();
    assert!(
        !format!("{auth:?}").contains("eyJ"),
        "Debug redacts the token"
    );
    assert!(parse_wham(b"nope").is_err());
}
