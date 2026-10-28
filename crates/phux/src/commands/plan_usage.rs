//! `phux agent usage`: the AI plan limits this host can see (Claude Code, Codex,
//! Grok Build), and with `--publish` the `phux.usage/v1` Global metadata key
//! the usage plugin keeps current for every client. The collection lives in
//! [`phux_usage`]; this verb only adds state, the refresh hint, and the
//! write. It never starts a server.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use phux_client::attach::AttachError;
use phux_client::global_metadata::{self, WriteError};
use phux_server::runtime::default_socket_path;
use phux_usage::{MAX_DOCUMENT_BYTES, Plan, REFRESH_KEY, Snapshot, USAGE_KEY};

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::server_target::ServerTarget;

/// Rewrite an unchanged document this often, so `observed_at_ms` keeps
/// telling clients the publisher is alive.
const REPUBLISH_AFTER_MS: i64 = 5 * 60_000;

/// What the server holds before this run.
#[derive(Debug, Default)]
struct Published {
    refresh: Option<String>,
    current: Option<Snapshot>,
}

/// `phux agent usage [--json] [--publish] [--refresh]`.
///
/// A publish takes the state lock before it touches the server or any
/// credential and holds it through the write, so an older snapshot can never
/// overwrite a newer one. A publish that finds the lock held, or that runs
/// within [`phux_usage::state::RUN_INTERVAL_MS`] of the last one, does
/// nothing: hooks fire in bursts and must not queue on each other.
pub(crate) fn run_usage(
    json: bool,
    publish: bool,
    refresh: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let now_ms = now_ms();
    let dir = phux_config::instance::state_dir().join("usage");
    let opened = if publish {
        phux_usage::Store::try_open(&dir)
    } else {
        phux_usage::Store::open(&dir).map(Some)
    };
    let (store, mut state) = match opened {
        Ok(Some(opened)) => opened,
        Ok(None) => {
            info!("usage: another publish is running; nothing to do");
            return ExitCode::SUCCESS;
        }
        Err(err) => return json_err::emit(json, &state_error(&dir, &err), 1),
    };
    if publish && !refresh && state.ran_recently(now_ms) {
        info!("usage: published moments ago; nothing to do (pass --refresh to force)");
        return ExitCode::SUCCESS;
    }
    let target = ServerTarget::local(&socket.unwrap_or_else(default_socket_path));
    let published = if publish {
        match rt.block_on(read_published(&target)) {
            Ok(published) => published,
            Err(err) => return target.report_unreachable(json, &err, "usage"),
        }
    } else {
        Published::default()
    };
    let snapshot = collect(&mut state, now_ms, refresh, published.refresh.as_deref());
    if let Err(err) = store.save(&state) {
        warn!("usage: could not save state in {}: {err}", dir.display());
    }
    if publish && should_publish(published.current.as_ref(), &snapshot, refresh) {
        let bytes = match encode(&snapshot) {
            Ok(bytes) => bytes,
            Err(err) => return json_err::emit(json, &err, 1),
        };
        match rt.block_on(write(&target, bytes)) {
            Ok(()) => {}
            Err(WriteError::Refused(refusal)) => {
                let err = CliError::new(
                    codes::TRANSPORT,
                    format!("the server refused {USAGE_KEY}: {refusal}"),
                    "check this connection's grant covers `bind@global`",
                );
                return json_err::emit(json, &err, 1);
            }
            Err(WriteError::Attach(err)) => return target.report_unreachable(json, &err, "usage"),
        }
    }
    drop(store);
    if json {
        outln!("{}", serde_json::to_string(&snapshot).unwrap_or_default());
    } else {
        out!("{}", phux_usage::render_text(&snapshot, now_ms));
    }
    ExitCode::SUCCESS
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn state_error(dir: &std::path::Path, err: &std::io::Error) -> CliError {
    CliError::new(
        codes::TRANSPORT,
        format!("cannot open usage state at {}: {err}", dir.display()),
        "check the directory is writable",
    )
}

/// Collect against the locked state. A refresh hint the state has not seen
/// yet counts as `--refresh` once.
fn collect(
    state: &mut phux_usage::State,
    now_ms: i64,
    refresh: bool,
    hint: Option<&str>,
) -> Snapshot {
    let hinted = hint.is_some() && hint != state.refresh_seen.as_deref();
    if hinted {
        state.refresh_seen = hint.map(str::to_owned);
    }
    state.last_run_ms = now_ms;
    let host = phux_config::instance::local_host_name();
    phux_usage::collect(
        &phux_usage::Sources::from_env(),
        state,
        phux_usage::Request {
            host: &host,
            now_ms,
            eager: refresh || hinted,
            fetch: &phux_usage::http::Curl,
        },
    )
}

/// Write when a plan's numbers or status changed, the document is missing
/// or old, or the caller asked. A re-read that only moved `fetched_at_ms` is
/// not a change: every write reaches every watching phone.
fn should_publish(current: Option<&Snapshot>, next: &Snapshot, refresh: bool) -> bool {
    let Some(current) = current else {
        return true;
    };
    refresh
        || current.host != next.host
        || comparable(&current.plans) != comparable(&next.plans)
        || next.observed_at_ms - current.observed_at_ms >= REPUBLISH_AFTER_MS
}

/// Plans without their read times.
fn comparable(plans: &[Plan]) -> Vec<Plan> {
    plans
        .iter()
        .map(|plan| Plan {
            fetched_at_ms: None,
            ..plan.clone()
        })
        .collect()
}

fn encode(snapshot: &Snapshot) -> Result<Vec<u8>, CliError> {
    let bytes = serde_json::to_vec(snapshot).unwrap_or_default();
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(CliError::new(
            codes::TRANSPORT,
            format!(
                "the usage document is {} bytes, over the {MAX_DOCUMENT_BYTES}-byte limit",
                bytes.len()
            ),
            "report this as a bug",
        ));
    }
    Ok(bytes)
}

async fn read_published(target: &ServerTarget) -> Result<Published, AttachError> {
    let mut conn = target.connect().await?;
    let mut values = global_metadata::read(&mut conn, &[REFRESH_KEY, USAGE_KEY])
        .await?
        .into_iter();
    let refresh = values
        .next()
        .flatten()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let current = values
        .next()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Ok(Published { refresh, current })
}

async fn write(target: &ServerTarget, value: Vec<u8>) -> Result<(), WriteError> {
    let mut conn = target.connect().await?;
    global_metadata::write(&mut conn, USAGE_KEY, value).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(observed_at_ms: i64, used: f64) -> Snapshot {
        snapshot_read_at(observed_at_ms, used, 1)
    }

    fn snapshot_read_at(observed_at_ms: i64, used: f64, fetched_at_ms: i64) -> Snapshot {
        serde_json::from_value(serde_json::json!({
            "schema": "phux.usage/v1",
            "host": "studio",
            "observed_at_ms": observed_at_ms,
            "plans": [{
                "provider": "codex", "harness": "codex", "plan": "pro", "label": "Pro",
                "account": null, "source": "rollout", "status": "ok", "error": null,
                "fetched_at_ms": fetched_at_ms,
                "windows": [{"id": "7d", "label": "7d", "minutes": 10080, "used_percent": used, "resets_at_ms": null}]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn only_a_change_an_old_document_or_a_refresh_rewrites() {
        let old = snapshot(0, 1.0);
        assert!(should_publish(None, &old, false));
        assert!(!should_publish(Some(&old), &snapshot(60_000, 1.0), false));
        assert!(should_publish(Some(&old), &snapshot(60_000, 2.0), false));
        assert!(should_publish(
            Some(&old),
            &snapshot(REPUBLISH_AFTER_MS, 1.0),
            false
        ));
        assert!(should_publish(Some(&old), &snapshot(1, 1.0), true));
    }

    #[test]
    fn a_fresh_read_of_the_same_numbers_is_not_a_change() {
        let old = snapshot_read_at(0, 1.0, 1);
        assert!(!should_publish(
            Some(&old),
            &snapshot_read_at(60_000, 1.0, 60_000),
            false
        ));
        assert!(should_publish(
            Some(&old),
            &snapshot_read_at(60_000, 1.5, 60_000),
            false
        ));
    }

    #[test]
    fn the_document_stays_under_the_contract_limit() {
        assert!(encode(&snapshot(0, 1.0)).is_ok());
    }
}
