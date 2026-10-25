//! Per-pane frontend state and the client-local indices built over it:
//! `PaneSlot`, the session-kernel alias, the VCS index, and the attention
//! helpers. Shared vocabulary the driver and its siblings read; this module
//! reads nothing back from them (only [`super::outcome`] and
//! [`super::render`]).

use std::collections::HashMap;
use std::time::{Duration, Instant};

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
    /// Unjournaled `input_activity`: who last sent input, and when the badge
    /// started, so the rail can fade it.
    pub(super) input_origin: Option<InputOrigin>,
}

/// How long a fresh origin badge stays at full emphasis before it dims.
pub(super) const INPUT_ORIGIN_BRIGHT: Duration = Duration::from_millis(800);
/// How long the badge stays on the rail, including the dim tail.
pub(super) const INPUT_ORIGIN_LIFE: Duration = Duration::from_millis(1_600);

/// One transient origin badge.
pub(super) struct InputOrigin {
    /// The label from the event (`client_name`, or `client:<id>`).
    pub(super) label: String,
    /// When this badge started. A newer event replaces it and restarts the fade.
    pub(super) since: Instant,
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
            input_origin: None,
        })
    }

    /// Start (or restart) the origin badge.
    pub(super) fn note_input_origin(&mut self, label: String, now: Instant) {
        self.input_origin = Some(InputOrigin { label, since: now });
    }

    /// Delay until the next fade boundary, if the badge is still showing.
    pub(super) fn input_origin_delay(&self, now: Instant) -> Option<Duration> {
        let since = self.input_origin.as_ref()?.since;
        let age = now.saturating_duration_since(since);
        if age < INPUT_ORIGIN_BRIGHT {
            Some(INPUT_ORIGIN_BRIGHT.saturating_sub(age))
        } else if age < INPUT_ORIGIN_LIFE {
            Some(INPUT_ORIGIN_LIFE.saturating_sub(age))
        } else {
            None
        }
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
    kernel: &AttachKernel,
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
        origin: origin_badge(slot, Instant::now()),
        scrolled_back: published_terminal(kernel, id).and_then(rows_scrolled_back),
    })
}

/// Rows between the viewport's bottom and the live tail, `None` while the
/// viewport follows the tail (or at it).
fn rows_scrolled_back(terminal: &GhosttyTerminal<'_, '_>) -> Option<u64> {
    if terminal.viewport_active().unwrap_or(true) {
        return None;
    }
    let bar = terminal.scrollbar().ok()?;
    let rows = bar.total.saturating_sub(bar.offset.saturating_add(bar.len));
    (rows > 0).then_some(rows)
}

/// A name [`crate::layout::ArrangementDoc::default_group_name`] handed out
/// (a bare integer): it would read as a second, disagreeing index, so the
/// tab prefers context.
pub(in crate::attach) fn auto_named(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

/// The last path component of `cwd`, `~` for the home directory, `None`
/// for an empty path.
pub(in crate::attach) fn cwd_basename(cwd: &str) -> Option<String> {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if std::env::var_os("HOME").is_some_and(|home| home == trimmed) {
        return Some("~".to_owned());
    }
    trimmed
        .rsplit('/')
        .next()
        .filter(|base| !base.is_empty())
        .map(ToOwned::to_owned)
}

/// What a window is called wherever the chrome names it (the tab bar, the
/// tab picker): a user-given name as is; an auto-numbered one prefers
/// context, from the focused pane's agent, its title, then its cwd.
pub(in crate::attach) fn display_window_name(
    name: &str,
    focus: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
    agent_meta: &HashMap<ResourceId, phux_client::agent_meta::AgentRecord>,
) -> String {
    if !auto_named(name) {
        return name.to_owned();
    }
    let slot = focus.and_then(|fid| panes.get(fid));
    focus
        .and_then(|fid| agent_meta.get(fid))
        .map(|record| record.name.clone())
        .or_else(|| {
            slot.map(|slot| slot.last_title.trim())
                .filter(|title| !title.is_empty())
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            slot.and_then(|slot| slot.cwd.as_deref())
                .and_then(cwd_basename)
        })
        .unwrap_or_else(|| name.to_owned())
}

/// The origin badge still inside its lifetime, dim after the bright interval.
fn origin_badge(
    slot: &PaneSlot,
    now: Instant,
) -> Option<crate::render::chrome::dividers::OriginBadge<'_>> {
    let mark = slot.input_origin.as_ref()?;
    let age = now.saturating_duration_since(mark.since);
    if mark.label.is_empty() || age >= INPUT_ORIGIN_LIFE {
        return None;
    }
    Some(crate::render::chrome::dividers::OriginBadge {
        text: mark.label.as_str(),
        dim: age >= INPUT_ORIGIN_BRIGHT,
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

mod metadata;
pub(super) use metadata::*;

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
