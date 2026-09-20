//! Translation from owned runtime events into kernel inputs.

use super::{
    Adapter, AgentSessionDeclaration, CanonicalGeometry, EffectBuffer, EngineAdapter, EngineEvent,
    KernelInput, SessionKernel,
};
#[cfg(feature = "engine")]
use super::{FrameColors, Rgb};

#[cfg(feature = "engine")]
pub(super) fn frame_colors(colors: &libghostty_vt::render::Colors) -> FrameColors {
    let rgb = |color: libghostty_vt::style::RgbColor| Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    };
    FrameColors {
        background: rgb(colors.background),
        foreground: rgb(colors.foreground),
        cursor: colors.cursor.map(rgb),
        palette: Box::new(colors.palette.map(rgb)),
    }
}

type KernelResult =
    Result<(), phux_client_core::session::KernelError<<Adapter as EngineAdapter>::Error>>;

pub(super) fn apply_event(
    kernel: &mut SessionKernel<Adapter>,
    event: EngineEvent,
    effects: &mut EffectBuffer,
) -> KernelResult {
    match event {
        EngineEvent::AttachStarted {
            attach_id,
            terminals,
        } => kernel.update(
            KernelInput::AttachStarted {
                attach_id,
                terminals: &terminals,
            },
            effects,
        ),
        EngineEvent::AttachReady { attach_id } => {
            kernel.update(KernelInput::AttachReady { attach_id }, effects)
        }
        EngineEvent::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => kernel.update(
            KernelInput::BootstrapBegin {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                geometry: CanonicalGeometry { cols, rows },
                base_seq,
            },
            effects,
        ),
        EngineEvent::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => kernel.update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload: &payload,
            },
            effects,
        ),
        EngineEvent::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => kernel.update(
            KernelInput::BootstrapReady {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: history_cursor.as_deref(),
            },
            effects,
        ),
        event => apply_stream_event(kernel, event, effects),
    }
}

fn apply_stream_event(
    kernel: &mut SessionKernel<Adapter>,
    event: EngineEvent,
    effects: &mut EffectBuffer,
) -> KernelResult {
    match event {
        EngineEvent::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            rows,
            cursor,
            next_cursor,
            payload,
        } => kernel.update(
            KernelInput::HistoryPage {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                rows,
                payload: &payload,
                cursor: &cursor,
                next_cursor: next_cursor.as_deref(),
            },
            effects,
        ),
        EngineEvent::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
        } => kernel.update(
            KernelInput::HistoryTombstone {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: &cursor,
                reason,
            },
            effects,
        ),
        EngineEvent::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
            required_bytes,
            required_rows,
        } => kernel.update(
            KernelInput::HistoryRejected {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                cursor: &cursor,
                reason,
                required_bytes,
                required_rows,
            },
            effects,
        ),
        EngineEvent::Output {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => kernel.update(
            KernelInput::ResourceOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                payload: &bytes,
            },
            effects,
        ),
        event => apply_lifecycle_event(kernel, event, effects),
    }
}

fn apply_lifecycle_event(
    kernel: &mut SessionKernel<Adapter>,
    event: EngineEvent,
    effects: &mut EffectBuffer,
) -> KernelResult {
    match event {
        EngineEvent::Tombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        } => kernel.update(
            KernelInput::Tombstone {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            },
            effects,
        ),
        EngineEvent::Closed {
            terminal_id,
            exit_status,
            signal,
            reason,
        } => kernel.update(
            KernelInput::ResourceClosed {
                terminal_id: &terminal_id,
                exit_status,
                signal,
                reason,
            },
            effects,
        ),
        EngineEvent::Agent { terminal_id, event } => kernel.update(
            KernelInput::Event {
                terminal_id: &terminal_id,
                event: &event,
            },
            effects,
        ),
        EngineEvent::AgentSessionDeclared {
            terminal_id,
            parent,
            provider,
            native_id,
            state,
        } => kernel.update(
            KernelInput::AgentSessionDeclared(AgentSessionDeclaration {
                terminal_id: &terminal_id,
                parent: parent.as_ref(),
                provider: provider.as_deref(),
                native_id: native_id.as_deref(),
                state: state.as_deref(),
            }),
            effects,
        ),
        EngineEvent::AttachStarted { .. }
        | EngineEvent::AttachReady { .. }
        | EngineEvent::BootstrapBegin { .. }
        | EngineEvent::BootstrapChunk { .. }
        | EngineEvent::BootstrapReady { .. }
        | EngineEvent::HistoryPage { .. }
        | EngineEvent::HistoryTombstone { .. }
        | EngineEvent::HistoryRejected { .. }
        | EngineEvent::Output { .. } => Ok(()),
    }
}
