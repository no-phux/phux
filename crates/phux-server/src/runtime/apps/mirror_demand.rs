//! The mirrors a server keeps beside `[[mirrors]]`: on-demand ones and what
//! Rune's macOS runner reported (`runtime::apps::terminal::standing`).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use phux_config::MirrorConfigEntry;

/// A runner report older than this means Rune is gone.
pub(crate) const RUNNER_FRESH: Duration = Duration::from_secs(10);

/// The mirrors wanted beside `[[mirrors]]`, and Rune's macOS runner.
#[derive(Debug, Default)]
pub(crate) struct Demand {
    /// The `[[mirrors]]` entries the supervisor last read.
    pub(crate) config: Vec<MirrorConfigEntry>,
    /// Mirrors `MIRROR { Start }` asked for, by session name.
    pub(crate) started: BTreeMap<String, MirrorConfigEntry>,
    /// When Rune's runner last reported, and the targets it reported.
    pub(crate) runner: Option<(Instant, serde_json::Value)>,
}

impl Demand {
    /// Every mirror to keep running: `[[mirrors]]`, then the started ones
    /// no entry already names.
    #[must_use]
    pub(crate) fn wanted(&self) -> Vec<MirrorConfigEntry> {
        let mut wanted = self.config.clone();
        wanted.extend(
            self.started
                .iter()
                .filter(|(name, _)| !self.config.iter().any(|e| e.session_name() == **name))
                .map(|(_, entry)| entry.clone()),
        );
        wanted
    }

    /// The targets Rune's runner reported, unless it has gone quiet.
    pub(crate) fn fresh_targets(&self, now: Instant) -> Option<&serde_json::Value> {
        self.runner
            .as_ref()
            .filter(|(at, _)| now.saturating_duration_since(*at) < RUNNER_FRESH)
            .map(|(_, targets)| targets)
    }
}
