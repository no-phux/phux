//! The mirrors a server keeps beside `[[mirrors]]`: on-demand ones and what
//! Rune's macOS runner reported. `MIRROR` writes it and the standing-mirror
//! supervisor that runs them reads it, so it is the apps' slot, not one
//! app's.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use phux_config::MirrorConfigEntry;

use crate::state::{AppSlot, ServerState};

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

impl AppSlot for Demand {
    fn forget_resource(
        &mut self,
        _resource: phux_core::ids::ResourceId,
        _wire: Option<&phux_protocol::ids::ResourceId>,
    ) {
    }
}

/// The demand before anything asked for a mirror.
static NO_DEMAND: std::sync::LazyLock<Demand> = std::sync::LazyLock::new(Demand::default);

impl ServerState {
    /// The mirrors wanted beside `[[mirrors]]`, and Rune's macOS runner.
    #[must_use]
    pub(crate) fn mirror_demand(&self) -> &Demand {
        self.app_slot::<Demand>().unwrap_or(&NO_DEMAND)
    }

    /// Mutable [`Self::mirror_demand`].
    pub(crate) fn mirror_demand_mut(&mut self) -> &mut Demand {
        self.app_slot_mut::<Demand>()
    }
}
