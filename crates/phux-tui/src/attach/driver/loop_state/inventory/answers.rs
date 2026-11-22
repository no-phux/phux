//! What a host inventory reply says about each satellite, and when its
//! request is overdue.

use super::super::{HOST_INVENTORY_DEADLINE, Notice, SatelliteHost};

/// The held `SatelliteUnreachable` notices the inventory reply does not
/// explain (by exact diagnostic or by naming the row's host); those are news.
pub(in crate::attach::driver) fn unexplained_unreachable_notices(
    held: Vec<String>,
    hosts: &[phux_protocol::wire::info::HostInventory],
) -> Vec<String> {
    held.into_iter()
        .filter(|notice| {
            !hosts
                .iter()
                .any(|row| unreachable_row_explains(row, notice))
        })
        .collect()
}

/// True when `notice` is the diagnostic this unreachable host already reported.
fn unreachable_row_explains(row: &phux_protocol::wire::info::HostInventory, notice: &str) -> bool {
    let Some(diagnostic) = row.unreachable.as_deref() else {
        return false;
    };
    diagnostic == notice || notice.starts_with(&format!("satellite {} is unreachable", row.host))
}

/// Held `SatelliteUnreachable` diagnostics as status notices.
pub(in crate::attach::driver) fn federation_notices(messages: Vec<String>) -> Vec<Notice> {
    messages.into_iter().map(Notice::warn).collect()
}

/// What one host inventory said about each satellite.
#[derive(Debug, Default)]
pub(in crate::attach::driver) struct HostAnswers {
    /// Satellites the inventory reached.
    pub(in crate::attach::driver) reachable: Vec<SatelliteHost>,
    /// Satellites it could not list.
    pub(in crate::attach::driver) unreachable: Vec<SatelliteHost>,
}

/// Partition an inventory into the hosts it reached and the ones it could not list.
pub(in crate::attach::driver) fn host_answers(
    rows: &[phux_protocol::wire::info::HostInventory],
) -> HostAnswers {
    let (reachable, unreachable): (Vec<_>, Vec<_>) =
        rows.iter().partition(|row| row.is_reachable());
    let names = |rows: Vec<&phux_protocol::wire::info::HostInventory>| {
        rows.into_iter().map(|row| row.host.clone()).collect()
    };
    HostAnswers {
        reachable: names(reachable),
        unreachable: names(unreachable),
    }
}

/// Whether a host-inventory request sent at `since` is past the deadline.
pub(in crate::attach::driver) fn host_inventory_overdue(
    since: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    since.is_some_and(|sent| now.saturating_duration_since(sent) > HOST_INVENTORY_DEADLINE)
}
