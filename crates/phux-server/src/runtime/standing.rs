//! What the server keeps standing (`[[mirrors]]` and `[[producers]]`,
//! `docs/resource-kinds.md#standing-mirrors`): where it reads the entries,
//! and each entry's restart bookkeeping. The app that runs them keeps them
//! running ([`crate::runtime::apps::keep_standing`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use phux_config::{MirrorConfigEntry, ProducerConfigEntry};

use crate::hub::link::Backoff;

/// What the supervisor keeps: `[[mirrors]]` and `[[producers]]`.
pub type StandingEntries = (Vec<MirrorConfigEntry>, Vec<ProducerConfigEntry>);

/// How the supervisor reads its entries, at start and on every reload.
#[derive(Clone)]
pub struct MirrorSource(Arc<dyn Fn() -> Result<StandingEntries, String> + Send + Sync>);

impl std::fmt::Debug for MirrorSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MirrorSource(..)")
    }
}

impl MirrorSource {
    /// Wrap the function a start and a reload call. A reload error keeps
    /// the running mirrors.
    #[must_use]
    pub fn new(read: impl Fn() -> Result<StandingEntries, String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(read))
    }

    /// The entries it names now.
    pub(crate) fn read(&self) -> Result<StandingEntries, String> {
        (self.0)()
    }
}

/// How often the supervisor looks for a mirror that exited.
pub(super) const TICK: Duration = Duration::from_secs(2);
/// A mirror that ran this long clears its failure streak.
const HEALTHY_AFTER: Duration = Duration::from_secs(60);
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// One entry's restart bookkeeping; Rune's `phux agent surface standing`
/// keeps the same for the mirrors it runs on macOS.
#[derive(Debug)]
pub struct Slot {
    backoff: Backoff,
    /// When this supervisor last started the mirror; `None` for one it
    /// found running (a hot upgrade) or has not started.
    started: Option<Instant>,
    /// Not before this instant may it start again.
    next: Option<Instant>,
    /// The arguments it was started with; a reload that changes them
    /// restarts it.
    args: Option<Vec<String>>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            backoff: Backoff::new(BACKOFF_BASE, BACKOFF_CAP),
            started: None,
            next: None,
            args: None,
        }
    }
}

impl Slot {
    /// Record a start at `now` with `args`.
    pub fn started(&mut self, now: Instant, args: Vec<String>) {
        self.started = Some(now);
        self.args = Some(args);
    }

    /// Whether a running mirror was started with other arguments than
    /// `args`, so a reload changed it.
    #[must_use]
    pub fn changed(&self, args: &[String]) -> bool {
        self.args.as_deref().is_some_and(|old| old != args)
    }

    /// Whether to start the mirror now, given whether it runs.
    pub fn due(&mut self, alive: bool, now: Instant) -> bool {
        if alive {
            return false;
        }
        if let Some(started) = self.started.take() {
            if now.saturating_duration_since(started) >= HEALTHY_AFTER {
                self.backoff.reset();
            }
            self.next = Some(now + self.backoff.next_delay());
        }
        self.next.is_none_or(|next| now >= next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mirror_starts_at_once_and_restarts_with_backoff() {
        let t0 = Instant::now();
        let mut slot = Slot::default();
        assert!(slot.due(false, t0), "first start is immediate");
        slot.started = Some(t0);
        assert!(!slot.due(true, t0 + TICK), "running: nothing to do");
        // Dies quickly: waits 1 s, then 2 s after the next quick death.
        let died = t0 + TICK;
        assert!(!slot.due(false, died));
        assert!(!slot.due(false, died + Duration::from_millis(900)));
        assert!(slot.due(false, died + BACKOFF_BASE));
        slot.started = Some(died + BACKOFF_BASE);
        let died = died + BACKOFF_BASE + TICK;
        assert!(!slot.due(false, died));
        assert!(!slot.due(false, died + BACKOFF_BASE));
        assert!(slot.due(false, died + BACKOFF_BASE * 2));
        // A healthy run clears the streak.
        slot.started = Some(died);
        let died = died + HEALTHY_AFTER;
        assert!(!slot.due(false, died));
        assert!(slot.due(false, died + BACKOFF_BASE));
    }

    #[test]
    fn a_mirror_found_running_is_adopted_not_restarted() {
        let mut slot = Slot::default();
        assert!(!slot.due(true, Instant::now()));
        assert!(slot.started.is_none() && slot.args.is_none());
    }
}
