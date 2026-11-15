//! Builders shared by the session kernel's test modules: a [`Gen`] names one
//! replica generation and builds the inputs addressed to it; a [`Rig`] owns a
//! kernel and the effect buffer each update writes into.

#![allow(clippy::unwrap_used, reason = "tests")]

use phux_protocol::wire::frame::{CloseReason, TombstoneReason};
use phux_protocol::{BootstrapId, BootstrapStreamProfile, ResourceId, StreamId};

use super::{
    EffectBuffer, HistoryRejectionReason, KernelError, KernelInput, SessionKernel, TombstoneRecord,
};
use crate::engine::{CanonicalGeometry, EngineAdapter};
use crate::history::HistoryLoadState;

pub(super) fn terminal(raw: u32) -> ResourceId {
    ResourceId::local(raw)
}

pub(super) fn stream(raw: u64) -> StreamId {
    let Some(id) = StreamId::new(raw) else {
        panic!("test stream id must be non-zero");
    };
    id
}

pub(super) fn bootstrap(raw: u64) -> BootstrapId {
    let Some(id) = BootstrapId::new(raw) else {
        panic!("test bootstrap id must be non-zero");
    };
    id
}

pub(super) fn geometry() -> CanonicalGeometry {
    let Some(geometry) = CanonicalGeometry::new(80, 24) else {
        panic!("test geometry must be non-empty");
    };
    geometry
}

pub(super) const fn attach_started(attach_id: u32, terminals: &[ResourceId]) -> KernelInput<'_> {
    KernelInput::AttachStarted {
        attach_id,
        terminals,
    }
}

pub(super) const fn attach_ready<'a>(attach_id: u32) -> KernelInput<'a> {
    KernelInput::AttachReady { attach_id }
}

pub(super) const fn begin_input(
    terminal_id: &ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    geometry: CanonicalGeometry,
    base_seq: u64,
) -> KernelInput<'_> {
    KernelInput::BootstrapBegin {
        terminal_id,
        stream_id,
        bootstrap_id,
        profile,
        geometry,
        base_seq,
    }
}

pub(super) const fn chunk<'a>(
    terminal_id: &'a ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    chunk_seq: u32,
    payload: &'a [u8],
) -> KernelInput<'a> {
    KernelInput::BootstrapChunk {
        terminal_id,
        stream_id,
        bootstrap_id,
        chunk_seq,
        payload,
    }
}

pub(super) const fn ready<'a>(
    terminal_id: &'a ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    history_cursor: Option<&'a [u8]>,
) -> KernelInput<'a> {
    KernelInput::BootstrapReady {
        terminal_id,
        stream_id,
        bootstrap_id,
        history_cursor,
    }
}

pub(super) const fn closed(terminal_id: &ResourceId) -> KernelInput<'_> {
    KernelInput::ResourceClosed {
        terminal_id,
        exit_status: None,
        signal: None,
        reason: CloseReason::Unknown,
    }
}

pub(super) const fn output<'a>(
    terminal_id: &'a ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    seq: u64,
    payload: &'a [u8],
) -> KernelInput<'a> {
    KernelInput::ResourceOutput {
        terminal_id,
        stream_id,
        bootstrap_id,
        seq,
        payload,
    }
}

/// Whether any effect in `$effects` matches the pattern (and guard).
macro_rules! any_effect {
    ($effects:expr, $($pattern:tt)+) => {
        $effects.as_slice().iter().any(|effect| matches!(effect, $($pattern)+))
    };
}
pub(super) use any_effect;

/// The `(terminal, stream, bootstrap)` triple naming one replica generation,
/// with builders for the inputs addressed to it.
#[derive(Clone)]
pub(super) struct Gen {
    pub(super) id: ResourceId,
    pub(super) s: StreamId,
    pub(super) b: BootstrapId,
}

pub(super) fn gen_of(id: u32, s: u64, b: u64) -> Gen {
    Gen {
        id: terminal(id),
        s: stream(s),
        b: bootstrap(b),
    }
}

impl Gen {
    /// The same terminal and stream under another bootstrap generation.
    pub(super) fn with_bootstrap(&self, raw: u64) -> Self {
        Self {
            b: bootstrap(raw),
            ..self.clone()
        }
    }

    pub(super) fn begin(&self, base_seq: u64) -> KernelInput<'_> {
        self.begin_as(BootstrapStreamProfile::SynthesizedVtRaw, base_seq)
    }

    pub(super) fn begin_as(
        &self,
        profile: BootstrapStreamProfile,
        base_seq: u64,
    ) -> KernelInput<'_> {
        begin_input(&self.id, self.s, self.b, profile, geometry(), base_seq)
    }

    /// A bootstrap for a gridless resource; the wire encodes that as 0x0.
    pub(super) const fn begin_gridless(
        &self,
        profile: BootstrapStreamProfile,
        base_seq: u64,
    ) -> KernelInput<'_> {
        KernelInput::BootstrapBegin {
            terminal_id: &self.id,
            stream_id: self.s,
            bootstrap_id: self.b,
            profile,
            geometry: CanonicalGeometry { cols: 0, rows: 0 },
            base_seq,
        }
    }

    pub(super) const fn chunk<'a>(&'a self, seq: u32, payload: &'a [u8]) -> KernelInput<'a> {
        chunk(&self.id, self.s, self.b, seq, payload)
    }

    pub(super) const fn ready<'a>(&'a self, cursor: Option<&'a [u8]>) -> KernelInput<'a> {
        ready(&self.id, self.s, self.b, cursor)
    }

    pub(super) const fn output<'a>(&'a self, seq: u64, payload: &'a [u8]) -> KernelInput<'a> {
        output(&self.id, self.s, self.b, seq, payload)
    }

    /// History page `page_seq` 1 for `cursor`.
    pub(super) const fn page<'a>(
        &'a self,
        cursor: &'a [u8],
        next_cursor: Option<&'a [u8]>,
        payload: &'a [u8],
        rows: u32,
    ) -> KernelInput<'a> {
        KernelInput::HistoryPage {
            terminal_id: &self.id,
            stream_id: self.s,
            bootstrap_id: self.b,
            cursor,
            next_cursor,
            payload,
            page_seq: 1,
            rows,
        }
    }

    pub(super) const fn rejected<'a>(
        &'a self,
        cursor: &'a [u8],
        reason: HistoryRejectionReason,
        required_bytes: u32,
        required_rows: u32,
    ) -> KernelInput<'a> {
        KernelInput::HistoryRejected {
            terminal_id: &self.id,
            stream_id: self.s,
            bootstrap_id: self.b,
            cursor,
            reason,
            required_bytes,
            required_rows,
        }
    }

    pub(super) const fn tombstone(
        &self,
        reason: TombstoneReason,
        last_valid_seq: u64,
    ) -> KernelInput<'_> {
        KernelInput::Tombstone {
            terminal_id: &self.id,
            stream_id: self.s,
            bootstrap_id: self.b,
            reason,
            last_valid_seq,
        }
    }
}

/// A kernel and the effect buffer every update writes into.
pub(super) struct Rig<E: EngineAdapter> {
    pub(super) k: SessionKernel<E>,
    pub(super) fx: EffectBuffer,
}

impl<E: EngineAdapter> Rig<E> {
    pub(super) fn of(k: SessionKernel<E>) -> Self {
        Self {
            k,
            fx: EffectBuffer::new(),
        }
    }

    pub(super) fn up(&mut self, input: KernelInput<'_>) -> Result<(), KernelError<E::Error>> {
        self.k.update(input, &mut self.fx)
    }

    #[track_caller]
    pub(super) fn ok(&mut self, input: KernelInput<'_>) {
        self.up(input).unwrap();
    }

    pub(super) fn tomb(&self, g: &Gen) -> Option<TombstoneRecord> {
        self.k.tombstone(&g.id, g.s, g.b)
    }

    pub(super) fn history_state(&self, id: &ResourceId) -> HistoryLoadState {
        self.k.history_cache(id).unwrap().status().state
    }
}
