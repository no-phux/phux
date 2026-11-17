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
        let title = String::from_utf8_lossy(terminal.title()?);
        if Some(title.as_ref()) == self.reported_title.as_deref() {
            return Ok(());
        }
        let title = title.into_owned();
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

    /// Update local pixel metrics without changing authoritative grid geometry.
    pub fn set_cell_pixels(&mut self, width: u16, height: u16) -> Result<(), GhosttyEngineError> {
        let terminal = match &mut self.state {
            ReplicaState::Synthesized { terminal, .. } => terminal,
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::Finished(terminal) => terminal,
                NativeDecoderState::Ready(decoder) => decoder.terminal_mut(),
                _ => return Err(GhosttyEngineError::LiveOutputBeforeReady),
            },
        };
        // Local cell pitch is not a PTY geometry change. libghostty's resize
        // clears DEC 2026 even with unchanged rows/columns; keep the app's
        // transaction intact while a new view installs its pixel metrics.
        let held = terminal.mode(libghostty_vt::mode::Mode::SYNC_OUTPUT)?;
        terminal.resize(
            terminal.cols()?,
            terminal.rows()?,
            u32::from(width.max(1)),
            u32::from(height.max(1)),
        )?;
        terminal.set_mode(libghostty_vt::mode::Mode::SYNC_OUTPUT, held)?;
        Ok(())
    }

    /// Release a timed-out DEC 2026 hold. Reset the engine mode itself so a
    /// later start marker is a new transaction, even inside one output payload.
    ///
    /// # Errors
    ///
    /// The replica is not ready, or libghostty rejects the mode change.
    pub fn release_render_hold(&mut self) -> Result<(), GhosttyEngineError> {
        let terminal = match &mut self.state {
            ReplicaState::Synthesized { terminal, .. } => terminal,
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::Finished(terminal) => terminal,
                NativeDecoderState::Ready(decoder) => decoder.terminal_mut(),
                _ => return Err(GhosttyEngineError::LiveOutputBeforeReady),
            },
        };
        terminal.set_mode(libghostty_vt::mode::Mode::SYNC_OUTPUT, false)?;
        Ok(())
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

    /// Clear locally without losing the live parser's unfinished input.
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
                clear_terminal_presentation(terminal)?;
            }
            ReplicaState::Native(native) => native.clear_presentation()?,
        }
        Ok(())
    }
}

fn clear_terminal_presentation(
    terminal: &mut GhosttyTerminal<'_, '_>,
) -> Result<(), GhosttyEngineError> {
    // Export before ANY mutation. If tracking overflowed, refusing this local
    // operation preserves both the last good display and the pending parser.
    let continuation = terminal
        .continuation_alloc(None)
        .map_err(|error| match error {
            libghostty_vt::Error::InvalidValue => GhosttyEngineError::ClearContinuationUnavailable,
            other => GhosttyEngineError::Terminal(other),
        })?;
    terminal.set_selection(None)?;
    // CAN cancels VT strings and flushes an incomplete UTF-8 decoder. Any
    // replacement glyph produced by the latter is erased before replay.
    // A cancelled DCS query may produce a failure reply internally: replicas
    // deliberately have no PTY writer, so it is dropped like every other query.
    terminal.vt_write(b"\x18\x1b[H\x1b[2J\x1b[3J");
    terminal.scroll_viewport(ScrollViewport::Bottom);
    if let Some(bytes) = continuation {
        terminal.vt_write(&bytes);
    }
    Ok(())
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
    pub(super) history_framing: phux_protocol::snapshot_framing::HistoryFraming,
    pub(super) history_failed: bool,
}

impl NativeReplica {
    fn clear_presentation(&mut self) -> Result<(), GhosttyEngineError> {
        if !self.protocol_finished {
            return Err(GhosttyEngineError::LiveOutputBeforeReady);
        }
        match &mut self.decoder {
            NativeDecoderState::Finished(terminal) => {
                clear_terminal_presentation(terminal)?;
            }
            NativeDecoderState::Ready(decoder) => {
                clear_terminal_presentation(decoder.terminal_mut())?;
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

#[cfg(test)]
mod clear_tests {
    use super::*;
    use libghostty_vt::point::{Coordinate, Point};
    use std::cell::RefCell;

    #[test]
    fn unavailable_continuation_refuses_clear_without_losing_display_or_input() {
        let mut terminal = GhosttyTerminal::new(40, 3).unwrap();
        terminal.set_continuation_max_bytes(2).unwrap();
        terminal.vt_write(b"KEPT\x1b]2;pending ");
        assert!(matches!(
            clear_terminal_presentation(&mut terminal),
            Err(GhosttyEngineError::ClearContinuationUnavailable)
        ));
        let cell = terminal
            .grid_ref(Point::Active(Coordinate { x: 0, y: 0 }))
            .unwrap();
        assert_eq!(cell.cell().unwrap().codepoint().unwrap(), u32::from(b'K'));
        // Completing the original pending OSC proves refusal did not inject CAN.
        terminal.vt_write(b"title\x07");
        assert_eq!(terminal.title().unwrap(), b"pending title");
        clear_terminal_presentation(&mut terminal).unwrap();
        let cell = terminal
            .grid_ref(Point::Active(Coordinate { x: 0, y: 0 }))
            .unwrap();
        assert_eq!(cell.cell().unwrap().codepoint().unwrap(), 0);
    }

    #[test]
    fn clearing_replays_pending_dcs_query_with_original_rendition() {
        let replies = RefCell::new(Vec::new());
        let mut terminal = GhosttyTerminal::new(20, 2).unwrap();
        terminal.set_continuation_max_bytes(4096).unwrap();
        terminal
            .on_pty_write(|_, bytes| replies.borrow_mut().extend_from_slice(bytes))
            .unwrap();
        terminal.vt_write(b"old\r\nhistory\r\nvisible\x1b[1m\x1bP$q");
        clear_terminal_presentation(&mut terminal).unwrap();
        // CAN unhooks DECRQSS and can produce a failure response. Client
        // replicas never install this PTY sink, so it cannot reach the peer.
        assert_eq!(&*replies.borrow(), b"\x1bP0$r\x1b\\");
        replies.borrow_mut().clear();
        assert_eq!(terminal.scrollback_rows().unwrap(), 0);
        terminal.vt_write(b"m\x1b\\");
        assert_eq!(&*replies.borrow(), b"\x1bP1$r0;1m\x1b\\");
    }

    #[test]
    fn clearing_replays_partial_kitty_command_without_transmitting_it_early() {
        let mut terminal = GhosttyTerminal::new(20, 2).unwrap();
        terminal.set_continuation_max_bytes(4096).unwrap();
        phux_vt::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal).unwrap();
        terminal.vt_write(b"visible\x1b_Ga=T,f=32,s=1,v=1,i=42,q=2;/wAA");
        clear_terminal_presentation(&mut terminal).unwrap();
        terminal.vt_write(b"/w==\x1b\\");
        let graphics = terminal.kitty_graphics().unwrap().unwrap();
        assert_eq!(
            graphics.image(42).unwrap().data().unwrap(),
            Some([255, 0, 0, 255].as_slice())
        );
    }
}
