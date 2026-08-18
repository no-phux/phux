//! Per-pane frontend state and the client-local indices built over it.
//!
//! Lifted out of [`super::driver`] under phux-4fbs.4. `PaneSlot`, the
//! session-kernel alias, the VCS index, and the attention helpers are shared
//! vocabulary that the driver and half a dozen siblings both read — they are
//! not the driver's lifecycle state (`published_terminal` and
//! `clear_attention_on_input` have no production call site in the driver at
//! all). Declaring them here keeps the dependency one-way: `paint`,
//! `server_frame`, `input_dispatch`, `rendered`, and `fleet` read this module,
//! and this module reads nothing back from any of them.
//!
//! Depends on nothing inside `attach` except [`super::outcome`] and
//! [`super::render`].

use std::collections::HashMap;

use libghostty_vt::Terminal as GhosttyTerminal;
#[cfg(test)]
use libghostty_vt::TerminalOptions;
use libghostty_vt::terminal::Mode;
use phux_client_core::engine::ghostty::GhosttyAdapter;
#[cfg(test)]
use phux_client_core::session::EffectBuffer as KernelEffectBuffer;
use phux_client_core::session::SessionKernel;
#[cfg(test)]
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::ids::{ClientId, TerminalId};
use phux_protocol::wire::frame::TerminalLifecycle;

use super::outcome::AttachError;
use super::render::{ReplicaWalk, TerminalRenderer};
use crate::predict::PredictionState;

/// Fallback per-cell pixel size for client-side libghostty mirrors.
///
/// The server-side actor uses the same conventional 8x16 default until a real
/// viewport pixel report arrives. The client mirror also needs nonzero cell
/// pixels: classic Kitty placements without explicit `c/r` dimensions infer
/// their grid footprint from pixel geometry, and a zero value makes the first
/// live render skip the placement until a later snapshot supplies `c/r`.
#[cfg(test)]
pub(super) const FALLBACK_CELL_PX: (u32, u32) = (8, 16);

pub(super) type AttachKernel = SessionKernel<GhosttyAdapter>;

pub(super) fn published_terminal<'a>(
    kernel: &'a AttachKernel,
    terminal_id: &TerminalId,
) -> Option<&'a GhosttyTerminal<'static, 'static>> {
    kernel.published_engine(terminal_id)?.terminal()
}

/// The published replica `Terminal` for one pane, paired with the generation
/// token the pane's renderer must walk it under.
///
/// Any path that hands the terminal to a [`TerminalRenderer`] walk fetches
/// through this funnel: the kernel REPLACES the published `Terminal` when a
/// replica generation is republished, and the renderer's pooled render state
/// discards its cache exactly when this token changes — even at unchanged
/// geometry (`phux-994s`). Returning the two halves as one [`ReplicaWalk`] is
/// what keeps them from drifting apart on the way to the renderer. Paths that
/// only inspect the terminal (modes, title, input routing) may keep using
/// [`published_terminal`].
pub(super) fn published_replica<'a>(
    kernel: &'a AttachKernel,
    terminal_id: &TerminalId,
) -> Option<ReplicaWalk<'a, 'static, 'static>> {
    let replica = kernel.published(terminal_id)?;
    let terminal = replica.engine().terminal()?;
    Some(ReplicaWalk {
        terminal,
        generation: replica.key().generation_token(),
    })
}

/// Driver-owned state for client-local attention navigation (phux-oih5.16).
///
/// The first jump saves the pane the user came from. Further cycling leaves
/// that origin untouched; return consumes it even when the pane has gone
/// stale. Nothing in this state is serialized or written to metadata.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct AttentionNavigation {
    origin: Option<TerminalId>,
}

impl AttentionNavigation {
    /// Save an origin only when a navigation excursion is not already active.
    pub(super) fn save_origin_once(&mut self, origin: Option<&TerminalId>) {
        if self.origin.is_none() {
            self.origin = origin.cloned();
        }
    }

    /// Consume the saved origin. A stale origin must not remain armed forever.
    pub(super) const fn take_origin(&mut self) -> Option<TerminalId> {
        self.origin.take()
    }
}

/// One pane's render and frontend-local metadata.
/// Production terminal ownership lives exclusively in the connection's
/// `SessionKernel<GhosttyAdapter>`. The test-only terminal keeps existing
/// isolated renderer policy tests independent from wire bootstrap fixtures.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-pane flags (scroll, attention, sync-output, seen); a bitset would obscure every read site"
)]
pub(super) struct PaneSlot {
    #[cfg(test)]
    /// Isolated terminal fixture; production uses the session kernel replica.
    pub terminal: GhosttyTerminal<'static, 'static>,
    /// Cached render scaffolding. One per pane so libghostty's iterators
    /// stay warm across frames (the renderer's `last_cursor` is also
    /// per-pane, so each pane's predictive-echo anchor is independent).
    pub renderer: TerminalRenderer<'static>,
    /// Server-authored canonical grid dimensions for prediction and layout metadata.
    pub geometry: (u16, u16),
    /// ADR-0033 supervisory lifecycle for this pane, driven by inbound
    /// `TerminalControl` events: `Running` until a `Freeze` (SIGSTOP) flips it
    /// to `Frozen`. Read at paint time to render the "FROZEN" chrome badge.
    pub lifecycle: TerminalLifecycle,
    /// ADR-0033 input-lease holder for this pane (the wire `ClientId` that has
    /// "the wheel"), or `None` when the pane is `Open`. Compared against the
    /// driver's own `ClientId` to render "you" vs another client.
    pub input_holder: Option<ClientId>,
    /// phux-i0e8.2.1: `true` once this slot has folded at least one
    /// `TerminalControl` event. The first one a slot sees is the
    /// attach-time initial state (the server re-states the lease on
    /// subscribe) and must NOT raise the input-authority status-bar
    /// notice; only later holder changes are transitions worth calling
    /// out.
    pub control_seen: bool,
    /// `true` while the client-local viewport is (possibly) scrolled up into
    /// scrollback — set by wheel / copy-mode scrolls, cleared when a key press
    /// headed for the pane snaps the viewport back to the live screen (tmux
    /// behavior). Without the snap, a scrolled viewport stays pinned in
    /// scrollback forever and the pane looks frozen: new output (e.g. the
    /// shell prompt after a TUI app exits) lands below the visible rows.
    pub viewport_scrolled: bool,
    /// phux-foz.1: `true` when an agent in this pane is waiting on a human
    /// answer. Set by an inbound ADR-0035 `AgentEvent::Asked`; cleared when
    /// the user sends key/paste input to the pane (see
    /// [`clear_attention_on_input`]). Read at chrome-paint time for the
    /// window tab marker and the status-bar attention hint.
    pub attention: bool,
    /// Start of the current DEC synchronized-output transaction (`?2026h`).
    pub sync_output_since: Option<tokio::time::Instant>,
    /// Whether mirror state changed during the transaction.
    pub sync_output_dirty: bool,
    /// phux-foz.4: the pane's working directory as the server last
    /// announced it — seeded from the `ATTACHED` snapshot's
    /// `TerminalInfo.cwd` (the spawn cwd) and refined by `cwd_changed`
    /// events. `None` until either lands. Projected into the status-bar
    /// `cwd` widget when this pane is focused.
    pub cwd: Option<String>,
    /// phux-foz.4: exit code of the last command that finished in this
    /// pane (`command_finished.exit_code`, OSC-133 `D` mark). `None`
    /// before the first command finishes or when the shell reported no
    /// code. Projected into the status-bar `exit` widget when focused.
    pub last_exit: Option<i32>,
    /// phux-foz.9: the OSC 0/2 title as of the last chrome-relevant engine
    /// apply, cached from the published replica so title transitions are cheap.
    /// is the ONLY identity signal a plain `claude`/`codex` pane emits
    /// (no `phux.agent/v1` record, no ADR-0035 events), and it arrives
    /// as ordinary `TERMINAL_OUTPUT` bytes — without this diff the
    /// sidebar's agents section (and the window-tab labels, phux-efj7)
    /// would only refresh on an unrelated chrome event. Empty ⇒ no
    /// title set, matching libghostty's `title()` contract.
    pub last_title: String,
    /// The attention ladder's "have you looked at this?" bit. Set whenever the
    /// pane is the focused one (every loop iteration), cleared whenever an
    /// UNFOCUSED pane's `phux.agent/v1` record changes.
    ///
    /// This is what lets the sidebar rank "finished, unread" above "still
    /// working": an agent that goes `done` in a background pane re-arms as
    /// unseen and climbs the strip until the user actually visits it. Starts
    /// `false` — a pane you have never focused has never been reviewed.
    pub seen: bool,
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
            let mut terminal = GhosttyTerminal::new(TerminalOptions {
                cols: cols.max(1),
                rows: rows.max(1),
                max_scrollback: 10_000,
            })?;
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
            lifecycle: TerminalLifecycle::Running,
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
        })
    }

    /// Allocate a fresh slot with a conservative placeholder size.
    /// Prefer [`Self::new_with_size`] whenever the attach snapshot,
    /// viewport, or layout already tells us the pane's real dimensions.
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

/// Build a protocol-0.7 test session with atomically published synthesized replicas.
///
/// Test helpers must seed terminal state through the same ATTACHED,
/// BEGIN/CHUNK/READY, and `ATTACH_READY` transitions as production rather than
/// mutating the test-only [`PaneSlot::terminal`] compatibility field.
#[cfg(test)]
pub(super) fn published_test_state(
    entries: &[(&TerminalId, u16, u16, &[u8])],
) -> (
    AttachKernel,
    KernelEffectBuffer,
    HashMap<TerminalId, PaneSlot>,
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

/// phux-p4vp: the driver's per-pane workspace metadata — each pane's
/// working directory (from the `ATTACHED` snapshot's `TerminalInfo::cwd`)
/// plus the memoizing branch cache that turns a cwd into a VCS branch
/// label by reading `.git/HEAD` (see [`crate::vcs`]). Entirely
/// client-local: nothing here touches the wire or the server's actor
/// path, and lookups are cached file reads, never a `git` subprocess.
#[derive(Debug, Default)]
pub(super) struct VcsIndex {
    /// Pane → working directory, seeded from the `ATTACHED` snapshot.
    cwds: HashMap<TerminalId, std::path::PathBuf>,
    /// cwd → branch memo.
    cache: crate::vcs::BranchCache,
}

impl VcsIndex {
    /// Fold an `ATTACHED` snapshot's `(pane, cwd)` pairs into the index.
    /// The snapshot is authoritative for the panes it names; panes that no
    /// longer exist are dropped (re-attach hygiene).
    pub(super) fn apply_snapshot(&mut self, pane_cwds: Vec<(TerminalId, String)>) {
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
    pub(super) fn branch_for_pane(&mut self, pane: &TerminalId) -> Option<String> {
        let cwd = self.cwds.get(pane)?.clone();
        self.cache.branch_for(&cwd)
    }

    /// phux-foz.7: the VCS branch label for an explicit `cwd` (the fleet
    /// dashboard resolves against the pane's *live* cwd — snapshot-seeded
    /// and refined by `cwd_changed` events — rather than this index's
    /// snapshot-only map). Same memoized `.git/HEAD` read, never a
    /// subprocess.
    pub(super) fn branch_for_cwd(&mut self, cwd: &str) -> Option<String> {
        self.cache.branch_for(std::path::Path::new(cwd))
    }
}

/// Re-anchor predictive echo to a newly focused published terminal.
pub(super) fn reanchor_predict_to_pane(
    predict: &mut PredictionState,
    panes: &HashMap<TerminalId, PaneSlot>,
    fid: &TerminalId,
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

/// phux-foz.1: clear a pane's asked-attention flag because the user sent it
/// input (the clearing rule documented in `docs/consumers/tui.md`). Returns
/// `true` when the flag actually flipped, so the caller can schedule a
/// chrome repaint only on a real transition.
pub(super) fn clear_attention_on_input(
    panes: &mut HashMap<TerminalId, PaneSlot>,
    pane: &TerminalId,
) -> bool {
    match panes.get_mut(pane) {
        Some(slot) if slot.attention => {
            slot.attention = false;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::attach::render::ReplicaWalk;

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

    /// phux-994s: a republished replica generation at UNCHANGED geometry must
    /// serve fresh rows through an incremental paint — no `force_full`.
    ///
    /// Before the generation token, this was true only because every
    /// republish happened to be followed by `paint_full_frame` (whose
    /// `force_full=true` repaints every row) and because libghostty's
    /// viewport-pin comparison usually notices a swapped terminal — an
    /// allocator-dependent accident, not a contract. Like the phux-5pyx
    /// resize test in `attach::render`, this is a contract lock rather than
    /// a deterministic fails-without-the-fix guard (the pin comparison can
    /// mask the stale cache unless the old allocation is recycled); the
    /// deterministic guard lives in `phux_protocol::render_pool::tests`.
    /// What this test pins is the client wiring: the kernel's republish
    /// changes the token `published_replica` hands out, and the renderer's
    /// pooled state honours it end to end.
    #[test]
    fn republish_at_same_geometry_serves_fresh_rows_without_force_full() {
        use phux_client_core::engine::CanonicalGeometry;
        use phux_client_core::session::KernelInput;
        use phux_protocol::{BootstrapId, BootstrapStreamProfile, StreamId};

        let id = TerminalId::local(1);
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

        // Republish: a second bootstrap generation for the same stream at
        // the SAME geometry, carrying different content. The kernel stages
        // and atomically publishes a NEW libghostty Terminal.
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

        // Simulate an unrelated walk (the structured-cells projection, a
        // grapheme read) consuming the fresh replica's terminal-side dirty
        // state before the paint path gets there — the ordering that made
        // the pre-token coupling dangerous.
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

    /// phux-oih5.16: the driver holds exactly one client-local origin. A
    /// second attention jump cannot overwrite it, and return consumes it.
    #[test]
    fn attention_navigation_saves_once_and_consumes() {
        let mut navigation = AttentionNavigation::default();
        navigation.save_origin_once(Some(&TerminalId::local(1)));
        navigation.save_origin_once(Some(&TerminalId::local(2)));
        assert_eq!(navigation.take_origin(), Some(TerminalId::local(1)));
        assert_eq!(navigation.take_origin(), None);
    }

    /// phux-foz.1: key/paste input forwarded to a pane clears its asked
    /// flag exactly once — the transition reports `true`, repeats and
    /// unknown panes report `false` (no spurious chrome repaints).
    #[test]
    fn clear_attention_on_input_clears_once() {
        let id = TerminalId::local(1);
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
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
            !clear_attention_on_input(&mut panes, &TerminalId::local(9)),
            "unknown pane reports no transition"
        );
    }
}
