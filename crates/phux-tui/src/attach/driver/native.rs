//! A single-terminal viewer rendered by the host emulator, with no TUI state.
use super::terminal::{RawModeGuard, install_panic_hook_once};
use crate::attach::tty_input::TtyInput;
use phux_client::attach::{AttachError, connection::Connection, input::StdinParser};
use phux_protocol::{
    caps::{
        BootstrapProfileKind, BootstrapProfileSet, BootstrapStreamProfile, ClientCapabilities,
        Layer, LayerSet,
    },
    ids::{BootstrapId, ResourceId, StreamId},
    input::{
        InputEvent,
        key::{KeyAction, ModSet, PhysicalKey},
    },
    wire::frame::{Command, CommandResult, CommandValue, FrameKind, ResourceLifecycle, StateScope},
};
use std::{
    io::{self, IsTerminal, Write},
    time::Duration,
};
use tokio::signal::unix::{SignalKind, signal};

/// Dial a local server with the native emulator's synthesized VT/raw offer.
/// Native checkpoints and state-sync deltas cannot be printed by an emulator.
///
/// # Errors
/// Transport, negotiation, or development-profile isolation failures.
pub async fn connect(socket: &std::path::Path) -> Result<Connection, AttachError> {
    let mut caps = ClientCapabilities::new().with_layers(LayerSet::with(&[Layer::L3]));
    caps.bootstrap.profiles = BootstrapProfileSet::with(&[BootstrapProfileKind::SynthesizedVtRaw]);
    Connection::connect_with_hello(socket, "phux-native-terminal".into(), caps).await
}

/// Display exactly `id`, routing every input and resize only to that resource.
/// The caller negotiates synthesized VT/raw and retains connection ownership.
///
/// # Errors
/// Terminal setup, transport, bootstrap integrity or server refusal failures.
pub async fn run(conn: &mut Connection, id: ResourceId) -> Result<(), AttachError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(AttachError::NotATty);
    }
    // Register before raw mode, including HUP from an emulator closing its PTY.
    let mut winch = signal(SignalKind::window_change())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut term = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut quit = signal(SignalKind::quit())?;
    let mut suspend = signal(SignalKind::from_raw(rustix::process::Signal::TSTP.as_raw()))?;
    phux_crash::install_terminal_restore_only();
    install_panic_hook_once();
    let mut out = io::stdout();
    let _raw = RawModeGuard::install_native(&mut out)?;
    // Raw app output may enable any tracking mode, independent of TUI policy.
    let _reset = NativeReset;
    out.write_all(b"\x1b[?25h")?;
    let mut input = TtyInput::open();
    let mut parser = StdinParser::new();
    let mut bytes = [0; 8192];
    let mut stream = Output::default();
    // Settle this exact PTY before taking the bootstrap cut. Re-read after
    // attachment too, since the window can change during negotiation.
    resize(conn, &id).await?;
    conn.send(&FrameKind::Command {
        request_id: 1,
        command: Command::attach_terminal(id.clone(), None),
    })
    .await?;
    let result = async {
        let mut attached = false;
        let mut closed = false;
        let mut esc_deadline = tokio::time::Instant::now();
        let attach_deadline = tokio::time::sleep(Duration::from_secs(15));
        tokio::pin!(attach_deadline);
        loop {
            let events = tokio::select! {
                () = &mut attach_deadline, if !attached => return Err(AttachError::Protocol("native terminal attach timed out".into())),
                read = input.read(&mut bytes), if attached => {
                    let n = read?;
                    if n == 0 { break; }
                    esc_deadline = tokio::time::Instant::now() + Duration::from_millis(30);
                    parser.feed(&bytes[..n])
                }
                () = tokio::time::sleep_until(esc_deadline), if parser.esc_pending() && attached => parser.flush(),
                _ = winch.recv() => { if attached { resize(conn, &id).await?; } continue; }
                _ = hup.recv() => break,
                _ = term.recv() => break,
                _ = interrupt.recv() => break,
                _ = quit.recv() => break,
                // A stopped client would leave its emulator raw. Treat external
                // job-control suspension as detach; typed Ctrl-Z still goes to PTY.
                _ = suspend.recv() => break,
                frame = conn.recv() => {
                    let frame = frame?;
                    match frame {
                        FrameKind::CommandResult { request_id: 1, result: CommandResult::Ok } => {
                            attached = true;
                            if closed { break; }
                            resize(conn, &id).await?;
                            // A short-lived process can exit before we subscribe.
                            // Check after bootstrap so its retained final screen
                            // is displayed even when no live close event follows.
                            conn.send(&FrameKind::Command { request_id: 3, command: Command::GetState { scope: StateScope::Server } }).await?;
                        }
                        FrameKind::CommandResult { request_id: 3, result: CommandResult::OkWith(CommandValue::State(state)) } => {
                            if !state.resources.iter().any(|resource| resource.id == id && resource.lifecycle != ResourceLifecycle::Exited) { break; }
                        }
                        FrameKind::CommandResult { result: CommandResult::Error { message, .. }, .. }
                        | FrameKind::Error { message, .. } => return Err(AttachError::Refused(message)),
                        FrameKind::ResourceClosed { terminal_id, .. } if terminal_id == id => {
                            if attached { break; }
                            closed = true;
                        }
                        other => { stream.apply(&id, other, &mut out)?; }
                    }
                    continue;
                }
            };
            for event in events {
                if detach(&event) { return Ok(()); }
                conn.send(&input_frame(&id, event)?).await?;
            }
        }
        Ok(())
    }.await;
    // Closing this connection also drops the subscription. Never kill a process,
    // change session focus, or replay uncertain input after transport loss.
    let _ = tokio::time::timeout(
        Duration::from_millis(200),
        conn.send(&FrameKind::Command {
            request_id: 2,
            command: Command::DetachResource { terminal_id: id },
        }),
    )
    .await;
    result
}

struct NativeReset;
impl Drop for NativeReset {
    fn drop(&mut self) {
        let _ = io::stdout()
            .write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?25h\x1b[0m");
    }
}

async fn resize(conn: &mut Connection, id: &ResourceId) -> Result<(), AttachError> {
    let view = super::viewport::current_viewport()?;
    if view.cols == 0 || view.rows == 0 {
        return Ok(());
    }
    conn.send(&FrameKind::ResizeTerminal {
        terminal_id: id.clone(),
        cols: view.cols,
        rows: view.rows,
        cell_px: view
            .pixel_w
            .zip(view.pixel_h)
            .map(|(w, h)| (w / view.cols, h / view.rows))
            .filter(|&(w, h)| w > 0 && h > 0),
    })
    .await
}

fn detach(event: &InputEvent) -> bool {
    matches!(event, InputEvent::Key(key) if key.action != KeyAction::Release
        && key.key == PhysicalKey::BracketRight && key.mods == ModSet::CTRL)
}

fn input_frame(id: &ResourceId, event: InputEvent) -> Result<FrameKind, AttachError> {
    Ok(match event {
        InputEvent::Key(event) => FrameKind::InputKey {
            terminal_id: id.clone(),
            event,
        },
        InputEvent::Mouse(event) => FrameKind::InputMouse {
            terminal_id: id.clone(),
            event,
        },
        InputEvent::Focus(event) => FrameKind::InputFocus {
            terminal_id: id.clone(),
            event,
        },
        InputEvent::Paste(event) => FrameKind::InputPaste {
            terminal_id: id.clone(),
            event,
        },
        _ => {
            return Err(AttachError::Protocol(
                "unsupported terminal input event".into(),
            ));
        }
    })
}

/// Validate one subscription's generations before exposing bytes to the emulator.
#[derive(Default)]
struct Output {
    generation: Option<(StreamId, BootstrapId)>,
    chunk: u32,
    seq: u64,
    ready: bool,
}
impl Output {
    fn apply(
        &mut self,
        id: &ResourceId,
        frame: FrameKind,
        out: &mut impl Write,
    ) -> Result<(), AttachError> {
        let invalid = || {
            AttachError::Protocol("invalid native terminal output generation or sequence".into())
        };
        match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                base_seq,
                ..
            } if &terminal_id == id => {
                if profile != BootstrapStreamProfile::SynthesizedVtRaw {
                    return Err(invalid());
                }
                self.generation = Some((stream_id, bootstrap_id));
                self.chunk = 0;
                self.seq = base_seq;
                self.ready = false;
            }
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } if &terminal_id == id => {
                if self.generation != Some((stream_id, bootstrap_id))
                    || self.ready
                    || chunk_seq != self.chunk
                {
                    return Err(invalid());
                }
                self.chunk = self.chunk.checked_add(1).ok_or_else(invalid)?;
                out.write_all(&payload)?;
                out.flush()?;
            }
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } if &terminal_id == id => {
                if self.generation != Some((stream_id, bootstrap_id)) || self.ready {
                    return Err(invalid());
                }
                self.ready = true;
            }
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } if &terminal_id == id => {
                if self.generation == Some((stream_id, bootstrap_id)) {
                    self.ready = false;
                }
            }
            FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } if &terminal_id == id => {
                if self.generation != Some((stream_id, bootstrap_id))
                    || !self.ready
                    || self.seq.checked_add(1) != Some(seq)
                {
                    return Err(invalid());
                }
                self.seq = seq;
                out.write_all(&bytes)?;
                out.flush()?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    fn begin(id: &ResourceId) -> FrameKind {
        FrameKind::BootstrapBegin {
            terminal_id: id.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 10,
        }
    }
    fn ready(id: &ResourceId) -> FrameKind {
        FrameKind::BootstrapReady {
            terminal_id: id.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            history_cursor: None,
        }
    }
    fn output(id: &ResourceId, seq: u64) -> FrameKind {
        FrameKind::ResourceOutput {
            terminal_id: id.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            seq,
            bytes: b"live"[..].into(),
        }
    }
    #[test]
    fn bootstrap_and_live_output_are_targeted_and_contiguous() {
        let id = ResourceId::Local { id: 1 };
        let other = ResourceId::Local { id: 2 };
        let mut state = Output::default();
        let mut bytes = Vec::new();
        state.apply(&id, begin(&id), &mut bytes).unwrap();
        let chunk = FrameKind::BootstrapChunk {
            terminal_id: id.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            chunk_seq: 0,
            payload: b"boot"[..].into(),
        };
        state.apply(&id, chunk, &mut bytes).unwrap();
        state.apply(&id, ready(&id), &mut bytes).unwrap();
        state.apply(&id, output(&other, 999), &mut bytes).unwrap();
        state.apply(&id, output(&id, 11), &mut bytes).unwrap();
        assert_eq!(bytes, b"bootlive");
        assert!(state.apply(&id, output(&id, 13), &mut bytes).is_err());
        assert_eq!(bytes, b"bootlive", "never paint output after a gap");
        state.apply(&id, begin(&id), &mut bytes).unwrap();
        assert!(
            state.apply(&id, output(&id, 11), &mut bytes).is_err(),
            "new generation needs READY"
        );
    }
    #[test]
    fn input_and_detach_never_use_session_focus() {
        let id = ResourceId::Local { id: 27 };
        let mut parser = StdinParser::new();
        for event in parser.feed(b"hello\x03\x1b[A\x1b[200~paste\x1b[201~") {
            assert!(!detach(&event));
            let frame = input_frame(&id, event).unwrap();
            match frame {
                FrameKind::InputKey { terminal_id, .. }
                | FrameKind::InputPaste { terminal_id, .. } => assert_eq!(terminal_id, id),
                _ => unreachable!(),
            }
        }
        assert!(parser.feed(b"\x1d").iter().any(detach));
        assert!(
            !parser.feed(b"\x1b[200~\x1d\x1b[201~").iter().any(detach),
            "pasted detach byte is data"
        );
    }
}
