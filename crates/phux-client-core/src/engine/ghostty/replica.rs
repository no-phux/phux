//! Published and native replica state owned by the libghostty adapter.

use super::{
    BootstrapStreamProfile, DocumentAnchorId, EngineEffect, EngineEffectBuffer, FeedDecoder,
    FeedIncrementalDecoder, GhosttyEngineError, GhosttyTerminal, HashMap, PendingEffects,
    PhantomData, Rc, ScrollViewport, TerminalTheme, TrackedGridRef, apply_terminal_theme,
};

/// One adapter-owned libghostty replica.
///
/// ```compile_fail
/// fn require_thread_safe<T: Send + Sync>() {}
/// require_thread_safe::<phux_client_core::engine::ghostty::GhosttyReplica>();
/// ```
#[derive(Debug)]
pub struct GhosttyReplica {
    pub(super) pending: Rc<PendingEffects>,
    pub(super) profile: BootstrapStreamProfile,
    pub(super) reported_title: Option<String>,
    pub(super) anchors: HashMap<DocumentAnchorId, TrackedGridRef>,
    pub(super) state: ReplicaState,
    pub(super) history_max_bytes: Option<usize>,
    pub(super) history_max_lines: Option<usize>,
    pub(super) _not_send_or_sync: PhantomData<Rc<()>>,
}

impl GhosttyReplica {
    pub(super) fn publish_title(
        &mut self,
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), GhosttyEngineError> {
        let Some(terminal) = self.terminal() else {
            return Ok(());
        };
        let title = terminal.title()?;
        if Some(title) == self.reported_title.as_deref() {
            return Ok(());
        }
        let title = title.to_owned();
        self.reported_title = Some(title.clone());
        effects.push(EngineEffect::Status(crate::engine::EngineStatus::Title(
            title,
        )));
        Ok(())
    }

    /// Exact stream profile used to allocate this replica.
    #[must_use]
    pub const fn profile(&self) -> BootstrapStreamProfile {
        self.profile
    }

    /// Borrow the renderable terminal.
    ///
    /// Native replicas intentionally return `None` until authenticated READY
    /// has transferred the terminal and replayed its parser continuation.
    #[must_use]
    pub const fn terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
        match &self.state {
            ReplicaState::Synthesized { terminal, .. } => Some(terminal),
            ReplicaState::Native(native) => native.terminal(),
        }
    }

    pub(super) fn set_scrollback_max_bytes(
        &mut self,
        max: Option<usize>,
    ) -> Result<(), GhosttyEngineError> {
        match &mut self.state {
            ReplicaState::Synthesized { terminal, .. } => {
                terminal.set_scrollback_max_bytes(max)?;
            }
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::Finished(terminal) => {
                    terminal.set_scrollback_max_bytes(max)?;
                }
                NativeDecoderState::Ready(decoder) => {
                    decoder.terminal_mut().set_scrollback_max_bytes(max)?;
                }
                NativeDecoderState::Collecting(_) | NativeDecoderState::Failed => {}
            },
        }
        Ok(())
    }

    pub(super) fn set_scrollback_max_lines(
        &mut self,
        max: Option<usize>,
    ) -> Result<(), GhosttyEngineError> {
        match &mut self.state {
            ReplicaState::Synthesized { terminal, .. } => {
                terminal.set_scrollback_max_lines(max)?;
            }
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::Finished(terminal) => {
                    terminal.set_scrollback_max_lines(max)?;
                }
                NativeDecoderState::Ready(decoder) => {
                    decoder.terminal_mut().set_scrollback_max_lines(max)?;
                }
                NativeDecoderState::Collecting(_) | NativeDecoderState::Failed => {}
            },
        }
        Ok(())
    }

    /// Install `theme` as this replica's default colours (`None` clears them).
    /// A replica whose native bootstrap has not reached READY has no terminal
    /// yet and is left alone; it is themed when the bootstrap finishes.
    ///
    /// # Errors
    ///
    /// The libghostty option write that failed.
    pub fn apply_theme(&mut self, theme: Option<&TerminalTheme>) -> Result<(), GhosttyEngineError> {
        match &mut self.state {
            ReplicaState::Synthesized { terminal, .. } => apply_terminal_theme(terminal, theme),
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::Finished(terminal) => apply_terminal_theme(terminal, theme),
                NativeDecoderState::Ready(decoder) => {
                    apply_terminal_theme(decoder.terminal_mut(), theme)
                }
                NativeDecoderState::Collecting(_) | NativeDecoderState::Failed => Ok(()),
            },
        }
    }

    /// Apply a client-local viewport scroll without exposing mutable terminal ownership.
    pub fn scroll_viewport(&mut self, scroll: ScrollViewport) -> Result<(), GhosttyEngineError> {
        match &mut self.state {
            ReplicaState::Synthesized { terminal, .. } => terminal.scroll_viewport(scroll),
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::Finished(terminal) => terminal.scroll_viewport(scroll),
                NativeDecoderState::Ready(decoder) => {
                    decoder.terminal_mut().scroll_viewport(scroll);
                }
                NativeDecoderState::Collecting(_) | NativeDecoderState::Failed => {
                    return Err(GhosttyEngineError::LiveOutputBeforeReady);
                }
            },
        }
        Ok(())
    }

    /// Mutate screen/history directly; the live parser may be mid-sequence.
    pub(super) fn clear_presentation(&mut self) -> Result<(), GhosttyEngineError> {
        match &mut self.state {
            ReplicaState::Synthesized {
                terminal,
                protocol_finished,
                ..
            } => {
                if !*protocol_finished {
                    return Err(GhosttyEngineError::LiveOutputBeforeReady);
                }
                clear_terminal_presentation(terminal);
            }
            ReplicaState::Native(native) => native.clear_presentation()?,
        }
        Ok(())
    }
}

fn clear_terminal_presentation(terminal: &mut GhosttyTerminal<'_, '_>) {
    // Drive CUP/ED/selection on the grid directly. vt_write of those
    // sequences would complete a pending CSI/OSC/DCS or flush U+FFFD for
    // unfinished UTF-8; decoded snapshots also disable continuation tracking
    // so those bytes cannot be extracted and replayed after RIS.
    terminal.clear_presentation();
}

#[derive(Debug)]
pub(super) enum ReplicaState {
    Synthesized {
        terminal: GhosttyTerminal<'static, 'static>,
        protocol_finished: bool,
    },
    Native(NativeReplica),
}

#[derive(Debug)]
pub(super) struct NativeReplica {
    pub(super) pending: Rc<PendingEffects>,
    pub(super) decoder: NativeDecoderState,
    pub(super) protocol_finished: bool,
}

impl NativeReplica {
    fn clear_presentation(&mut self) -> Result<(), GhosttyEngineError> {
        if !self.protocol_finished {
            return Err(GhosttyEngineError::LiveOutputBeforeReady);
        }
        match &mut self.decoder {
            NativeDecoderState::Finished(terminal) => {
                clear_terminal_presentation(terminal);
            }
            NativeDecoderState::Ready(decoder) => {
                clear_terminal_presentation(decoder.terminal_mut());
            }
            NativeDecoderState::Collecting(_) | NativeDecoderState::Failed => {
                return Err(GhosttyEngineError::LiveOutputBeforeReady);
            }
        }
        Ok(())
    }

    const fn terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
        match &self.decoder {
            NativeDecoderState::Ready(decoder) => Some(decoder.terminal()),
            NativeDecoderState::Finished(terminal) => Some(terminal),
            NativeDecoderState::Collecting(_) | NativeDecoderState::Failed => None,
        }
    }
}

#[derive(Debug)]
pub(super) enum NativeDecoderState {
    Collecting(FeedDecoder<'static>),
    Ready(FeedIncrementalDecoder<'static, 'static>),
    Finished(GhosttyTerminal<'static, 'static>),
    Failed,
}
