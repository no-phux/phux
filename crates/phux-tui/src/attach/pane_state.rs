//! Per-pane frontend state and the client-local indices built over it:
//! `PaneSlot`, the session-kernel alias, the VCS index, and the attention
//! helpers. Shared vocabulary the driver and its siblings read; this module
//! reads nothing back from them (only [`super::outcome`] and
//! [`super::render`]).

use std::collections::HashMap;

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::terminal::Mode;
use phux_client_core::engine::ghostty::GhosttyAdapter;
#[cfg(test)]
use phux_client_core::session::EffectBuffer as KernelEffectBuffer;
use phux_client_core::session::SessionKernel;
#[cfg(test)]
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::ids::{ClientId, ResourceId};
use phux_protocol::wire::frame::ResourceLifecycle;

use super::outcome::AttachError;
use super::render::{ReplicaWalk, TerminalRenderer};
use crate::predict::PredictionState;

/// Fallback per-cell pixel size for client mirrors: nonzero, or Kitty
/// placements without `c/r` skip their first render.
#[cfg(test)]
pub(super) const FALLBACK_CELL_PX: (u32, u32) = (8, 16);

pub(super) type AttachKernel = SessionKernel<GhosttyAdapter>;

pub(super) fn published_terminal<'a>(
    kernel: &'a AttachKernel,
    terminal_id: &ResourceId,
) -> Option<&'a GhosttyTerminal<'static, 'static>> {
    kernel.published_engine(terminal_id)?.terminal()
}

/// A pane's published replica `Terminal` paired with its generation token.
/// Every renderer walk fetches through here: the kernel replaces the
/// `Terminal` on republish and the renderer drops its pooled cache exactly
/// when the token changes. Read-only inspection may use
/// [`published_terminal`].
pub(super) fn published_replica<'a>(
    kernel: &'a AttachKernel,
    terminal_id: &ResourceId,
) -> Option<ReplicaWalk<'a, 'static, 'static>> {
    let replica = kernel.published(terminal_id)?;
    let terminal = replica.engine().terminal()?;
    Some(ReplicaWalk {
        terminal,
        generation: replica.key().generation_token(),
    })
}

/// Client-local attention navigation: the first jump saves the origin pane,
/// cycling leaves it, return consumes it. Never serialized.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct AttentionNavigation {
    origin: Option<ResourceId>,
}

impl AttentionNavigation {
    /// Save an origin only when a navigation excursion is not already active.
    pub(super) fn save_origin_once(&mut self, origin: Option<&ResourceId>) {
        if self.origin.is_none() {
            self.origin = origin.cloned();
        }
    }

    /// Consume the saved origin. A stale origin must not remain armed forever.
    pub(super) const fn take_origin(&mut self) -> Option<ResourceId> {
        self.origin.take()
    }
}

/// One pane's render and frontend-local metadata. The terminal lives in the
/// session kernel; the test-only `terminal` serves isolated renderer tests.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-pane flags (scroll, attention, sync-output, seen); a bitset would obscure every read site"
)]
pub(super) struct PaneSlot {
    #[cfg(test)]
    /// Isolated terminal fixture; production uses the session kernel replica.
    pub terminal: GhosttyTerminal<'static, 'static>,
    /// Per-pane render scaffolding (warm iterators, predictive-echo anchor).
    pub renderer: TerminalRenderer<'static>,
    /// Server-authored canonical grid dimensions for prediction and layout metadata.
    pub geometry: (u16, u16),
    /// ADR-0033 lifecycle (`Frozen` renders the badge).
    pub lifecycle: ResourceLifecycle,
    /// ADR-0124: how the retained pane's process ended. `Some` makes it
    /// read-only with an "exited N" mark until `RESOURCE_CLOSED`.
    pub exited: Option<ExitMark>,
    /// ADR-0033 input-lease holder, or `None` when open.
    pub input_holder: Option<ClientId>,
    /// Whether a `TerminalControl` has been folded; the first is attach-time
    /// state and raises no notice.
    pub control_seen: bool,
    /// The viewport may be scrolled into scrollback; a key to the pane snaps
    /// it back (else new output lands below the visible rows).
    pub viewport_scrolled: bool,
    /// ADR-0035: an agent is waiting on a human; cleared by input to the pane
    /// ([`clear_attention_on_input`]).
    pub attention: bool,
    /// Start of the current DEC synchronized-output transaction (`?2026h`).
    pub sync_output_since: Option<tokio::time::Instant>,
    /// Whether mirror state changed during the transaction.
    pub sync_output_dirty: bool,
    /// The pane's cwd (snapshot-seeded, refined by `cwd_changed`), for the
    /// `cwd` widget.
    pub cwd: Option<String>,
    /// The last command's OSC-133 exit code, for the `exit` widget.
    pub last_exit: Option<i32>,
    /// The cached OSC 0/2 title (empty ⇒ none): the only identity a plain
    /// agent CLI emits, diffed so a change refreshes the chrome.
    pub last_title: String,
    /// The attention ladder's "looked at" bit: set while focused, cleared when
    /// an unfocused pane's agent record changes, so "finished, unread" ranks
    /// above "working" until visited.
    pub seen: bool,
    /// The satellite link is down: the leaf stays grey and input is dropped
    /// until an inventory says the host answers again.
    pub satellite_down: bool,
    /// Progressive history for the published replica is unavailable (pruned,
    /// tombstoned, rejected): only the scrollback this client already holds
    /// remains. Cleared when a fresh replica publishes or the cache reports
    /// a healthy state again; the focused pane badges it.
    pub history_degraded: bool,
    /// ADR-0147: `Some(title)` for the floating plugin overlay pane, which is
    /// in no layout window and paints in a box over the pane area.
    pub floating: Option<String>,
}

impl std::fmt::Debug for PaneSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneSlot").finish_non_exhaustive()
    }
}

impl PaneSlot {
    /// Allocate fresh frontend metadata and renderer scaffolding.
    pub(super) fn new_with_size(cols: u16, rows: u16) -> Result<Self, AttachError> {
        #[cfg(not(test))]
        let _ = (cols, rows);
        #[cfg(test)]
        let terminal = {
            let mut terminal = {
                let mut terminal = GhosttyTerminal::new(cols.max(1), rows.max(1))?;
                terminal.set_scrollback_max_lines(Some(10_000))?;
                terminal
            };
            phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal)?;
            terminal.resize(
                cols.max(1),
                rows.max(1),
                FALLBACK_CELL_PX.0,
                FALLBACK_CELL_PX.1,
            )?;
            terminal
        };
        Ok(Self {
            #[cfg(test)]
            terminal,
            renderer: TerminalRenderer::new()?,
            geometry: (cols.max(1), rows.max(1)),
            lifecycle: ResourceLifecycle::Running,
            exited: None,
            input_holder: None,
            control_seen: false,
            viewport_scrolled: false,
            attention: false,
            sync_output_since: None,
            sync_output_dirty: false,
            cwd: None,
            last_exit: None,
            last_title: String::new(),
            seen: false,
            satellite_down: false,
            history_degraded: false,
            floating: None,
        })
    }

    /// A slot at a placeholder size; prefer [`Self::new_with_size`].
    pub(super) fn new() -> Result<Self, AttachError> {
        Self::new_with_size(80, 24)
    }

    /// Update the cached title after a terminal mutation.
    pub(super) fn title_changed(&mut self, terminal: &GhosttyTerminal<'_, '_>) -> bool {
        let current = terminal.title().unwrap_or_default();
        if self.last_title == current {
            return false;
        }
        current.clone_into(&mut self.last_title);
        true
    }

    /// Refresh synchronized-output bookkeeping after a terminal mutation.
    pub(super) fn update_sync_output(
        &mut self,
        terminal: &GhosttyTerminal<'_, '_>,
        now: tokio::time::Instant,
    ) -> bool {
        let active = terminal.mode(Mode::SYNC_OUTPUT).unwrap_or(false);
        if active {
            self.sync_output_since.get_or_insert(now);
            self.sync_output_dirty = true;
        } else {
            self.sync_output_since = None;
            self.sync_output_dirty = false;
        }
        active
    }
}

/// The chrome label for one pane: its OSC title (no invented name) and its
/// ADR-0035 attention. Agent records are not read here.
pub(super) fn pane_label<'a>(
    panes: &'a HashMap<ResourceId, PaneSlot>,
    id: &ResourceId,
) -> Option<crate::render::chrome::dividers::PaneLabel<'a>> {
    let (key, slot) = panes.get_key_value(id)?;
    Some(crate::render::chrome::dividers::PaneLabel {
        text: slot.last_title.as_str(),
        agent: None,
        attention: slot.attention,
        seen: slot.seen,
        // Borrow the host from the map key so the label lives as long as
        // `panes`, the same lifetime as the title.
        host: key.host().map(phux_protocol::ids::SatelliteHost::as_str),
        unreachable: slot.satellite_down,
    })
}

/// Forget every pane's front buffer so its next paint rewrites dirty rows
/// whole; required after anything writes over pane interiors (see
/// `render::FrontBuffer`), and always safe.
pub(super) fn invalidate_all_fronts(panes: &mut HashMap<ResourceId, PaneSlot>) {
    for slot in panes.values_mut() {
        slot.renderer.invalidate_front();
    }
}

/// Forget the rows the predictive overlay painted over: its guesses are
/// healed only by a whole-row repaint, and a diff against the pre-guess
/// front row would strand the underline.
pub(super) fn invalidate_predicted_rows(slot: &mut PaneSlot, predict: &PredictionState) {
    for prediction in predict.pending() {
        slot.renderer
            .invalidate_front_rows(prediction.row..prediction.row.saturating_add(1));
    }
}

/// A test session with published synthesized replicas, seeded through the
/// same ATTACHED / bootstrap / `ATTACH_READY` path as production.
#[cfg(test)]
pub(super) fn published_test_state(
    entries: &[(&ResourceId, u16, u16, &[u8])],
) -> (
    AttachKernel,
    KernelEffectBuffer,
    HashMap<ResourceId, PaneSlot>,
) {
    use phux_client_core::session::KernelInput;
    use phux_protocol::{BootstrapId, BootstrapProfile, BootstrapStreamProfile, StreamId};

    let mut kernel = SessionKernel::new(
        GhosttyAdapter::new(BootstrapLimits::default()),
        BootstrapProfile::SynthesizedVtRaw,
    );
    let mut effects = KernelEffectBuffer::new();
    let terminals: Vec<_> = entries
        .iter()
        .map(|(terminal_id, ..)| (*terminal_id).clone())
        .collect();
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 1,
                terminals: &terminals,
            },
            &mut effects,
        )
        .expect("test ATTACHED");

    let stream_id = StreamId::new(1).expect("test stream");
    let bootstrap_id = BootstrapId::new(1).expect("test bootstrap");
    for (terminal_id, cols, rows, bytes) in entries {
        kernel
            .update(
                KernelInput::BootstrapBegin {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    profile: BootstrapStreamProfile::SynthesizedVtRaw,
                    geometry: phux_client_core::engine::CanonicalGeometry::new(*cols, *rows)
                        .expect("test geometry"),
                    base_seq: 0,
                },
                &mut effects,
            )
            .expect("test BOOTSTRAP_BEGIN");
        kernel
            .update(
                KernelInput::BootstrapChunk {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    chunk_seq: 0,
                    payload: bytes,
                },
                &mut effects,
            )
            .expect("test BOOTSTRAP_CHUNK");
        kernel
            .update(
                KernelInput::BootstrapReady {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    history_cursor: None,
                },
                &mut effects,
            )
            .expect("test BOOTSTRAP_READY");
    }
    kernel
        .update(KernelInput::AttachReady { attach_id: 1 }, &mut effects)
        .expect("test ATTACH_READY");

    let mut panes = HashMap::with_capacity(entries.len());
    for (terminal_id, cols, rows, _) in entries {
        let mut slot = PaneSlot::new_with_size(*cols, *rows).expect("test pane slot");
        let terminal = published_terminal(&kernel, terminal_id).expect("published test terminal");
        slot.title_changed(terminal);
        slot.update_sync_output(terminal, tokio::time::Instant::now());
        panes.insert((*terminal_id).clone(), slot);
    }
    (kernel, KernelEffectBuffer::new(), panes)
}

/// Per-pane cwds plus a memoized cwd -> branch cache (`.git/HEAD` reads,
/// never a subprocess). Client-local.
#[derive(Debug, Default)]
pub(super) struct VcsIndex {
    /// Pane → working directory, seeded from the `ATTACHED` snapshot.
    cwds: HashMap<ResourceId, std::path::PathBuf>,
    /// cwd → branch memo.
    cache: phux_client::vcs::BranchCache,
}

impl VcsIndex {
    /// Fold an `ATTACHED` snapshot's `(pane, cwd)` pairs; panes it omits are
    /// dropped.
    pub(super) fn apply_snapshot(&mut self, pane_cwds: Vec<(ResourceId, String)>) {
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
    pub(super) fn branch_for_pane(&mut self, pane: &ResourceId) -> Option<String> {
        let cwd = self.cwds.get(pane)?.clone();
        self.cache.branch_for(&cwd)
    }

    /// The branch for an explicit (live) `cwd`, same memoized read.
    pub(super) fn branch_for_cwd(&mut self, cwd: &str) -> Option<String> {
        self.cache.branch_for(std::path::Path::new(cwd))
    }
}

/// Re-anchor predictive echo to a newly focused published terminal.
pub(super) fn reanchor_predict_to_pane(
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
pub(super) fn clear_attention_on_input(
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
pub(super) struct ExitMark {
    /// `_exit(n)` status, when the server reported one.
    pub status: Option<i32>,
    /// The terminating signal, when the server reported one.
    pub signal: Option<i32>,
}

impl ExitMark {
    /// The mark for the exit facet an `ATTACHED` snapshot carries.
    pub(super) const fn from_facet(facet: &phux_protocol::wire::info::ExitFacet) -> Self {
        Self {
            status: facet.exit_status,
            signal: facet.signal,
        }
    }

    /// `exited 3`, `exited signal 9`, or `exited` when neither is known.
    pub(super) fn label(self) -> String {
        match (self.status, self.signal) {
            (Some(code), _) => format!("exited {code}"),
            (None, Some(signal)) => format!("exited signal {signal}"),
            (None, None) => "exited".to_owned(),
        }
    }

    /// Compact chrome fragment: `"3"`, `"sig9"`, or `""` when neither is known.
    pub(super) fn compact(self) -> String {
        match (self.status, self.signal) {
            (Some(code), _) => code.to_string(),
            (None, Some(signal)) => format!("sig{signal}"),
            (None, None) => String::new(),
        }
    }
}

/// ADR-0124: whether `pane` is retained after its process exited. Input to it
/// is dropped; its last grid stays on screen.
pub(super) fn pane_exited(panes: &HashMap<ResourceId, PaneSlot>, pane: &ResourceId) -> bool {
    panes.get(pane).is_some_and(|slot| slot.exited.is_some())
}

/// Keys and mouse reports to a satellite pane whose link is
/// down are dropped. Scrolling and copy-mode still read the last snapshot.
pub(super) fn pane_satellite_down(
    panes: &HashMap<ResourceId, PaneSlot>,
    pane: &ResourceId,
) -> bool {
    panes.get(pane).is_some_and(|slot| slot.satellite_down)
}

/// The host a hub `SatelliteUnreachable` diagnostic names (`satellite {host}
/// is unreachable: {why}`, L1 §9.1, or the shorter `... unreachable`).
pub(super) fn satellite_host_in_unreachable(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("satellite ")?;
    let host = rest.split_whitespace().next()?;
    (!host.is_empty() && rest.contains("unreachable")).then_some(host)
}

/// Mark every pane on `host` down. The layout is not touched. Returns
/// whether any pane changed, so chrome repaints only then.
pub(super) fn mark_satellite_down(panes: &mut HashMap<ResourceId, PaneSlot>, host: &str) -> bool {
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
pub(super) fn note_satellite_unreachable(
    panes: &mut HashMap<ResourceId, PaneSlot>,
    message: &str,
) -> bool {
    satellite_host_in_unreachable(message).is_some_and(|host| mark_satellite_down(panes, host))
}

/// Clear the down flag for `host` and return those panes so the driver can
/// `ATTACH_RESOURCE` them and replay the satellite's snapshot.
pub(super) fn satellite_panes_returned(
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::attach::render::ReplicaWalk;

    /// The hub's unreachable wording names a host, and only
    /// that host's panes go grey. A later return clears them for replay.
    #[test]
    fn satellite_unreachable_marks_one_host_and_a_return_clears_it() {
        let local = ResourceId::local(1);
        let edge = ResourceId::satellite("edge", 9);
        let other = ResourceId::satellite("gpubox", 3);
        let mut panes = HashMap::from([
            (local.clone(), PaneSlot::new().expect("slot")),
            (edge.clone(), PaneSlot::new().expect("slot")),
            (other.clone(), PaneSlot::new().expect("slot")),
        ]);
        assert_eq!(
            satellite_host_in_unreachable("satellite edge is unreachable: link is down"),
            Some("edge")
        );
        assert_eq!(
            satellite_host_in_unreachable("satellite gpubox unreachable"),
            Some("gpubox")
        );
        assert!(satellite_host_in_unreachable("the link dropped").is_none());
        assert!(note_satellite_unreachable(
            &mut panes,
            "satellite edge is unreachable: link is down"
        ));
        assert!(pane_satellite_down(&panes, &edge));
        assert!(!pane_satellite_down(&panes, &local));
        assert!(!pane_satellite_down(&panes, &other));
        assert_eq!(
            satellite_panes_returned(&mut panes, "edge"),
            vec![edge.clone()]
        );
        assert!(!pane_satellite_down(&panes, &edge));
    }

    #[test]
    fn pane_slot_initializes_nonzero_cell_pixels_for_live_kitty_render() {
        let mut slot = PaneSlot::new_with_size(10, 5).expect("slot");
        slot.terminal
            .vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=77,q=2;/wAA/w==\x1b\\");

        let mut out = Vec::new();
        slot.renderer
            .render(ReplicaWalk::for_test(&slot.terminal), &mut out)
            .expect("render");
        let replay = String::from_utf8_lossy(&out);
        assert!(
            replay.contains("\x1b_Ga=T,f=32,s=1,v=1,i=77,q=2,c=1,r=1,m=0;/wAA/w==\x1b\\"),
            "initial live render must replay classic Kitty placement; got {replay:?}"
        );
    }

    /// A republish at unchanged geometry must serve fresh rows through an
    /// incremental paint: the kernel's new token reaches the renderer. (The
    /// deterministic guard lives in `phux_protocol::render_pool::tests`.)
    #[test]
    fn republish_at_same_geometry_serves_fresh_rows_without_force_full() {
        use phux_client_core::engine::CanonicalGeometry;
        use phux_client_core::session::KernelInput;
        use phux_protocol::{BootstrapId, BootstrapStreamProfile, StreamId};

        let id = ResourceId::local(1);
        let (mut kernel, mut effects, mut panes) = published_test_state(&[(&id, 10, 2, b"AA")]);
        let slot = panes.get_mut(&id).expect("slot");

        let walk_1 = published_replica(&kernel, &id).expect("generation 1");
        let generation_1 = walk_1.generation;
        let mut first = Vec::new();
        let _ = slot
            .renderer
            .render_at(walk_1, &mut first, (0, 0), (10, 2))
            .expect("paint generation 1");
        assert!(
            String::from_utf8_lossy(&first).contains("AA"),
            "first incremental paint serves generation 1's rows"
        );

        // A second bootstrap generation, same geometry, new content.
        let stream_id = StreamId::new(1).expect("stream");
        let bootstrap_id = BootstrapId::new(2).expect("bootstrap 2");
        kernel
            .update(
                KernelInput::BootstrapBegin {
                    terminal_id: &id,
                    stream_id,
                    bootstrap_id,
                    profile: BootstrapStreamProfile::SynthesizedVtRaw,
                    geometry: CanonicalGeometry::new(10, 2).expect("geometry"),
                    base_seq: 0,
                },
                &mut effects,
            )
            .expect("republish BEGIN");
        kernel
            .update(
                KernelInput::BootstrapChunk {
                    terminal_id: &id,
                    stream_id,
                    bootstrap_id,
                    chunk_seq: 0,
                    payload: b"ZZ",
                },
                &mut effects,
            )
            .expect("republish CHUNK");
        kernel
            .update(
                KernelInput::BootstrapReady {
                    terminal_id: &id,
                    stream_id,
                    bootstrap_id,
                    history_cursor: None,
                },
                &mut effects,
            )
            .expect("republish READY");

        let walk_2 = published_replica(&kernel, &id).expect("generation 2");
        let generation_2 = walk_2.generation;
        assert_ne!(
            generation_1, generation_2,
            "a republish must change the walk-identity token"
        );

        // Another walk consumes the fresh replica's dirty state first.
        let mut thief = libghostty_vt::RenderState::new().expect("thief state");
        let _ = thief.update(walk_2.terminal).expect("thief update");

        let mut second = Vec::new();
        let _ = slot
            .renderer
            .render_at(walk_2, &mut second, (0, 0), (10, 2))
            .expect("paint generation 2");
        let painted = String::from_utf8_lossy(&second);
        assert!(
            painted.contains("ZZ"),
            "an incremental paint (no force_full) after a same-geometry \
             republish must serve the new generation's rows, got {painted:?}"
        );
    }

    /// The driver holds exactly one client-local origin. A
    /// second attention jump cannot overwrite it, and return consumes it.
    #[test]
    fn attention_navigation_saves_once_and_consumes() {
        let mut navigation = AttentionNavigation::default();
        navigation.save_origin_once(Some(&ResourceId::local(1)));
        navigation.save_origin_once(Some(&ResourceId::local(2)));
        assert_eq!(navigation.take_origin(), Some(ResourceId::local(1)));
        assert_eq!(navigation.take_origin(), None);
    }

    /// Input clears the asked flag once; repeats and unknown panes report
    /// false.
    #[test]
    fn clear_attention_on_input_clears_once() {
        let id = ResourceId::local(1);
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
        slot.attention = true;
        panes.insert(id.clone(), slot);

        assert!(clear_attention_on_input(&mut panes, &id), "first clear");
        assert!(
            !panes.get(&id).expect("slot").attention,
            "flag must be down after the clear"
        );
        assert!(
            !clear_attention_on_input(&mut panes, &id),
            "already-clear pane reports no transition"
        );
        assert!(
            !clear_attention_on_input(&mut panes, &ResourceId::local(9)),
            "unknown pane reports no transition"
        );
    }
}
