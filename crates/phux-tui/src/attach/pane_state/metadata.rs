use super::{HashMap, PaneSlot, PredictionState, ResourceId};
/// Per-pane cwds plus a memoized cwd -> branch cache (`.git/HEAD` reads,
/// never a subprocess). Client-local.
#[derive(Debug, Default)]
pub(in crate::attach) struct VcsIndex {
    /// Pane → working directory, seeded from the `ATTACHED` snapshot.
    cwds: HashMap<ResourceId, std::path::PathBuf>,
    /// cwd → branch memo.
    cache: phux_client::vcs::BranchCache,
}

impl VcsIndex {
    /// Fold an `ATTACHED` snapshot's `(pane, cwd)` pairs; panes it omits are
    /// dropped.
    pub(in crate::attach) fn apply_snapshot(&mut self, pane_cwds: Vec<(ResourceId, String)>) {
        if pane_cwds.is_empty() {
            return;
        }
        self.cwds = pane_cwds
            .into_iter()
            .map(|(id, cwd)| (id, std::path::PathBuf::from(cwd)))
            .collect();
    }

    /// The VCS branch label for `pane`'s working directory, or `None` when
    /// the cwd is unknown or not inside a repository.
    pub(in crate::attach) fn branch_for_pane(&mut self, pane: &ResourceId) -> Option<String> {
        let cwd = self.cwds.get(pane)?.clone();
        self.cache.branch_for(&cwd)
    }

    /// The branch for an explicit (live) `cwd`, same memoized read.
    pub(in crate::attach) fn branch_for_cwd(&mut self, cwd: &str) -> Option<String> {
        self.cache.branch_for(std::path::Path::new(cwd))
    }
}

/// Re-anchor predictive echo to a newly focused published terminal.
pub(in crate::attach) fn reanchor_predict_to_pane(
    predict: &mut PredictionState,
    panes: &HashMap<ResourceId, PaneSlot>,
    fid: &ResourceId,
) {
    let Some(slot) = panes.get(fid) else {
        predict.suspend();
        return;
    };
    let (cols, rows) = slot.geometry;
    if cols > 0 && rows > 0 {
        predict.set_viewport(cols, rows);
    } else {
        predict.clear();
    }
    match slot.renderer.last_cursor_local() {
        Some((row, col)) => predict.set_cursor(row, col),
        None => predict.suspend(),
    }
}

/// Clear a pane's asked flag on user input; true only on a real transition.
pub(in crate::attach) fn clear_attention_on_input(
    panes: &mut HashMap<ResourceId, PaneSlot>,
    pane: &ResourceId,
) -> bool {
    match panes.get_mut(pane) {
        Some(slot) if slot.attention => {
            slot.attention = false;
            true
        }
        _ => false,
    }
}

/// How a retained pane's process ended (ADR-0124), as the chrome shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::attach) struct ExitMark {
    /// `_exit(n)` status, when the server reported one.
    pub status: Option<i32>,
    /// The terminating signal, when the server reported one.
    pub signal: Option<i32>,
}

impl ExitMark {
    /// The mark for the exit facet an `ATTACHED` snapshot carries.
    pub(in crate::attach) const fn from_facet(
        facet: &phux_protocol::wire::info::ExitFacet,
    ) -> Self {
        Self {
            status: facet.exit_status,
            signal: facet.signal,
        }
    }

    /// `exited 3`, `exited signal 9`, or `exited` when neither is known.
    pub(in crate::attach) fn label(self) -> String {
        match (self.status, self.signal) {
            (Some(code), _) => format!("exited {code}"),
            (None, Some(signal)) => format!("exited signal {signal}"),
            (None, None) => "exited".to_owned(),
        }
    }

    /// Compact chrome fragment: `"3"`, `"sig9"`, or `""` when neither is known.
    pub(in crate::attach) fn compact(self) -> String {
        match (self.status, self.signal) {
            (Some(code), _) => code.to_string(),
            (None, Some(signal)) => format!("sig{signal}"),
            (None, None) => String::new(),
        }
    }
}

/// ADR-0124: whether `pane` is retained after its process exited. Input to it
/// is dropped; its last grid stays on screen.
pub(in crate::attach) fn pane_exited(
    panes: &HashMap<ResourceId, PaneSlot>,
    pane: &ResourceId,
) -> bool {
    panes.get(pane).is_some_and(|slot| slot.exited.is_some())
}

/// Keys and mouse reports to a satellite pane whose link is
/// down are dropped. Scrolling and copy-mode still read the last snapshot.
pub(in crate::attach) fn pane_satellite_down(
    panes: &HashMap<ResourceId, PaneSlot>,
    pane: &ResourceId,
) -> bool {
    panes.get(pane).is_some_and(|slot| slot.satellite_down)
}

/// What follows `satellite {host} ` in every `SatelliteUnreachable`
/// diagnostic the hub emits (`phux-server` `hub::relay`): the link-teardown
/// push, the fail-fast mailbox refusal, the dropped or silent reply, and the
/// cancelled proxy withdrawal. `ERROR` carries no host field, so this prose
/// is the only place the host appears.
const UNREACHABLE_TAILS: [&str; 5] = [
    "is unreachable",
    "link is down",
    "link dropped before ",
    "did not answer ",
    "proxy withdrawal cancelled ",
];

/// The host a hub `SatelliteUnreachable` diagnostic names (L1 §9.1), or
/// `None` when the message is not one of the shapes the hub emits.
pub(in crate::attach) fn satellite_host_in_unreachable(message: &str) -> Option<&str> {
    let (host, tail) = message.strip_prefix("satellite ")?.split_once(' ')?;
    (!host.is_empty() && UNREACHABLE_TAILS.iter().any(|t| tail.starts_with(t))).then_some(host)
}

/// Mark every pane on `host` down. The layout is not touched. Returns
/// whether any pane changed, so chrome repaints only then.
pub(in crate::attach) fn mark_satellite_down(
    panes: &mut HashMap<ResourceId, PaneSlot>,
    host: &str,
) -> bool {
    let mut changed = false;
    for (id, slot) in panes.iter_mut() {
        if id.host().is_some_and(|named| named.as_str() == host) && !slot.satellite_down {
            slot.satellite_down = true;
            changed = true;
        }
    }
    changed
}

/// Fold one `SatelliteUnreachable` message into pane state.
pub(in crate::attach) fn note_satellite_unreachable(
    panes: &mut HashMap<ResourceId, PaneSlot>,
    message: &str,
) -> bool {
    satellite_host_in_unreachable(message).is_some_and(|host| mark_satellite_down(panes, host))
}

/// Clear the down flag for `host` and return those panes so the driver can
/// `ATTACH_RESOURCE` them and replay the satellite's snapshot.
pub(in crate::attach) fn satellite_panes_returned(
    panes: &mut HashMap<ResourceId, PaneSlot>,
    host: &str,
) -> Vec<ResourceId> {
    let mut replay = Vec::new();
    for (id, slot) in panes.iter_mut() {
        if slot.satellite_down && id.host().is_some_and(|named| named.as_str() == host) {
            slot.satellite_down = false;
            replay.push(id.clone());
        }
    }
    replay
}
