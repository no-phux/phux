//! The audit log check.

use super::Check;
use crate::commands::cli_runtime;

/// Is the audit log being written? A failed write never stops a command,
/// so the server's failure count is the only sign (`docs/audit.md`).
pub(super) fn check_audit(socket_path: &std::path::Path) -> Check {
    let log = phux_server::audit::default_dir().join(phux_server::audit::LOG_FILE);
    match audit_write_failures(socket_path) {
        Some(failures) if failures > 0 => Check::warn(
            "audit",
            format!(
                "the server failed to write {failures} audit record(s) to {}",
                log.display()
            ),
            "commands still ran, unrecorded; the reason is in `phux logs --server` \
             (usually a full disk or a changed directory mode)",
        ),
        _ if log.exists() => Check::pass("audit", log.display().to_string()),
        _ => Check::pass(
            "audit",
            "no audit log yet; the server writes one from its first connection",
        ),
    }
}

/// The running server's `audit.write_failures` counter, if it answers.
fn audit_write_failures(socket_path: &std::path::Path) -> Option<u64> {
    if !socket_path.exists() {
        return None;
    }
    let report = cli_runtime().ok()?.block_on(async {
        let mut conn = phux_client::attach::connection::Connection::connect(socket_path)
            .await
            .ok()?;
        phux_client::state::get_perf_on(&mut conn, false).await.ok()
    })?;
    report
        .metrics
        .iter()
        .find(|metric| metric.name == "audit.write_failures")
        .and_then(|metric| match metric.value {
            phux_perf::MetricValue::Counter(count) => Some(count),
            _ => None,
        })
}
