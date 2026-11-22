//! Status-bar notices: shown on the bar, or traced without one.

use super::Notice;

/// Warn notices for every replay report that did not end in delivery.
pub(super) fn undelivered_notices(
    reports: impl IntoIterator<Item = crate::attach::input_replay::ReplayReport>,
) -> impl Iterator<Item = Notice> {
    reports
        .into_iter()
        .filter(|report| {
            !matches!(
                report.disposition,
                crate::attach::input_replay::ReplayDisposition::Delivered
            )
        })
        .map(|report| Notice::warn(report.notice_line()))
}

impl super::SessionLoop {
    /// Put `notices` on the status bar, or trace them when there is no bar.
    /// True when any was shown.
    pub(super) fn show_notices(&mut self, notices: impl IntoIterator<Item = Notice>) -> bool {
        let now = std::time::Instant::now();
        let mut shown = false;
        for notice in notices {
            if let Some(sb) = self.settings.status_bar.as_mut() {
                shown |= sb.set_notice(notice, now);
            } else {
                tracing::info!(
                    severity = ?notice.severity,
                    text = %notice.text,
                    "status-bar notice dropped: no status bar configured",
                );
            }
        }
        shown
    }
}
