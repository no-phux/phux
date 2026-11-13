//! `phux pair ls|prune|rotate|revoke`: edit the credential store, no server.

use std::process::ExitCode;

use super::PairAction;

pub(super) fn run_credential_action(
    tokens: &std::path::Path,
    action: PairAction,
    json: bool,
) -> ExitCode {
    match action {
        PairAction::Ls => run_pair_ls(tokens, json),
        PairAction::Prune { unused_for } => run_pair_prune(tokens, &unused_for, json),
        PairAction::Rotate {
            credential_id,
            overlap_seconds,
        } => run_pair_rotate(tokens, &credential_id, overlap_seconds, json),
        PairAction::Revoke { credential_id } => run_pair_revoke(tokens, &credential_id, json),
    }
}

fn run_pair_ls(tokens: &std::path::Path, json: bool) -> ExitCode {
    let rows = match phux_server::auth::list_credentials(tokens) {
        Ok(rows) => rows,
        Err(error) => {
            error!("pair ls: {error}");
            return ExitCode::FAILURE;
        }
    };
    let now = chrono::Utc::now();
    if json {
        let credentials: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.id,
                    "issued_at": row.issued_at.to_rfc3339(),
                    "last_seen": row.last_seen.map(|t| t.to_rfc3339()),
                    "revoked": row.revoked,
                    "generation": row.generation,
                    "expires_at": row.expires_at.map(|t| t.to_rfc3339()),
                    "link_expires_at": row.first_use_by.map(|t| t.to_rfc3339()),
                    "status": status(row, now).0,
                })
            })
            .collect();
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "operation": "ls",
            "tokens_path": tokens.display().to_string(),
            "credentials": credentials,
        }));
    }
    if rows.is_empty() {
        outln!("No credentials in {}.", tokens.display());
        return ExitCode::SUCCESS;
    }
    outln!(
        "{:<36}  {:<20}  {:<20}  {}",
        "ID",
        "MINTED",
        "LAST SEEN",
        "STATUS"
    );
    for row in rows {
        let minted = row.issued_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let seen = row.last_seen.map_or_else(
            || "never".to_owned(),
            |t| t.format("%Y-%m-%d %H:%M:%S").to_string(),
        );
        let (status, until) = status(&row, now);
        let until = until.map_or_else(String::new, |t| {
            format!(" (until {})", t.format("%Y-%m-%d %H:%M:%S"))
        });
        outln!(
            "{:<36}  {:<20}  {:<20}  {status}{until}",
            row.id,
            minted,
            seen
        );
    }
    ExitCode::SUCCESS
}

/// `revoked`, `expired`, `unused` (never admitted), or `active`, and the
/// instant it stops working when one is set (#2398).
fn status(
    row: &phux_server::auth::CredentialSummary,
    now: chrono::DateTime<chrono::Utc>,
) -> (&'static str, Option<chrono::DateTime<chrono::Utc>>) {
    let until = match (row.first_use_by, row.expires_at) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let status = if row.revoked {
        "revoked"
    } else if until.is_some_and(|until| now >= until) {
        "expired"
    } else if row.last_seen.is_none() {
        "unused"
    } else {
        "active"
    };
    (status, until.filter(|_| !row.revoked))
}

fn run_pair_prune(tokens: &std::path::Path, unused_for: &str, json: bool) -> ExitCode {
    let duration = match parse_unused_for(unused_for) {
        Ok(duration) => duration,
        Err(error) => {
            error!("pair prune: {error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = match phux_server::auth::prune_unused(tokens, duration) {
        Ok(outcome) => outcome,
        Err(error) => {
            error!("pair prune: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !outcome.is_durable() {
        warn!(
            "pair prune: revocation is active, but the store directory could not be synced; do not retry"
        );
    }
    if json {
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "operation": "prune",
            "unused_for": unused_for,
            "revoked": outcome.revoked_ids,
            "tokens_path": tokens.display().to_string(),
        }));
    }
    if outcome.revoked_ids.is_empty() {
        outln!("No unused credentials to prune in {}.", tokens.display());
    } else {
        outln!(
            "Revoked {} unused credential(s):",
            outcome.revoked_ids.len()
        );
        for id in &outcome.revoked_ids {
            outln!("  {id}");
        }
    }
    ExitCode::SUCCESS
}

fn run_pair_rotate(
    tokens: &std::path::Path,
    credential_id: &str,
    overlap_seconds: i64,
    json: bool,
) -> ExitCode {
    let overlap = chrono::Duration::seconds(overlap_seconds);
    let rotated = match phux_server::auth::rotate_credential(tokens, credential_id, overlap) {
        Ok(rotated) => rotated,
        Err(error) => {
            error!("pair rotate: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !rotated.is_durable() {
        warn!(
            "pair rotate: rotation is active, but the store directory could not be synced; do not retry"
        );
    }
    if json {
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "operation": "rotate",
            "credential_id": rotated.id,
            "generation": rotated.generation,
            "token": rotated.secret(),
            "overlap_seconds": overlap_seconds,
            "tokens_path": tokens.display().to_string(),
        }));
    }
    outln!(
        "Rotated credential {} to generation {}.",
        rotated.id,
        rotated.generation
    );
    outln!(
        "Previous generations remain valid for at most {overlap_seconds} seconds and never beyond their absolute expiry."
    );
    outln!(
        "Live sessions still on a previous generation are disconnected when that overlap ends; rotate with --overlap-seconds (up to 86400) to give devices longer to pick up the new token."
    );
    outln!();
    outln!("Pairing token (a secret — give it to the device once):");
    outln!("  {}", rotated.secret());
    outln!();
    outln!("Token written to {}", tokens.display());
    ExitCode::SUCCESS
}

fn run_pair_revoke(tokens: &std::path::Path, credential_id: &str, json: bool) -> ExitCode {
    // Bearer ids never carry the digest prefix; an enrolled certificate's
    // always does. Its revocation lives in the workload registry.
    if credential_id.starts_with("sha256:") {
        return crate::commands::workload::run(
            crate::commands::workload::WorkloadAction::Revoke {
                credential_id: credential_id.to_owned(),
            },
            json,
        );
    }
    let outcome = match phux_server::auth::revoke_credential(tokens, credential_id) {
        Ok(outcome) => outcome,
        Err(error) => {
            error!("pair revoke: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !outcome.is_durable() {
        warn!(
            "pair revoke: revocation is active, but the store directory could not be synced; do not retry"
        );
    }
    if json {
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "operation": "revoke",
            "credential_id": credential_id,
            "tokens_path": tokens.display().to_string(),
        }));
    }
    outln!("Revoked credential {credential_id} for new connections.");
    outln!("Established sessions remain active until disconnected.");
    ExitCode::SUCCESS
}

/// Parse `--expires`: same spellings as `--unused-for`, capped at 30 days.
pub(super) fn parse_expires(raw: &str) -> Result<chrono::Duration, String> {
    let duration = parse_duration(raw, "--expires")?;
    let max = chrono::Duration::days(30);
    if duration > max {
        return Err("--expires must be at most 30 days".to_owned());
    }
    Ok(duration)
}

/// Parse `--unused-for`: `30d`, `24h`, `90m`, `45s`, or a bare number of seconds.
fn parse_unused_for(raw: &str) -> Result<chrono::Duration, String> {
    parse_duration(raw, "--unused-for")
}

fn parse_duration(raw: &str, flag: &str) -> Result<chrono::Duration, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("{flag} needs a duration (e.g. 30d, 24h, 90m, 45s)"));
    }
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (digits, unit) = raw.split_at(split);
    if digits.is_empty() {
        return Err(format!("invalid {flag} duration: {raw:?}"));
    }
    let value: i64 = digits
        .parse()
        .map_err(|_| format!("invalid {flag} duration: {raw:?}"))?;
    if value <= 0 {
        return Err(format!("{flag} must be greater than zero"));
    }
    let seconds = match unit {
        "" | "s" => value,
        "m" => value.saturating_mul(60),
        "h" => value.saturating_mul(3600),
        "d" => value.saturating_mul(86400),
        _ => {
            return Err(format!("invalid {flag} unit in {raw:?}; use s, m, h, or d"));
        }
    };
    Ok(chrono::Duration::seconds(seconds))
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use phux_server::auth::CredentialSummary;

    #[test]
    fn ls_names_unused_expired_and_revoked_links() {
        let now = Utc::now();
        let row = CredentialSummary {
            id: "a".to_owned(),
            issued_at: now,
            last_seen: None,
            revoked: false,
            generation: 1,
            expires_at: None,
            first_use_by: Some(now + Duration::minutes(10)),
        };
        assert_eq!(super::status(&row, now), ("unused", row.first_use_by));
        let later = now + Duration::minutes(11);
        assert_eq!(super::status(&row, later).0, "expired");
        let paired = CredentialSummary {
            last_seen: Some(now),
            first_use_by: None,
            ..row.clone()
        };
        assert_eq!(super::status(&paired, later), ("active", None));
        let revoked = CredentialSummary {
            revoked: true,
            ..row
        };
        assert_eq!(super::status(&revoked, later), ("revoked", None));
    }
}
