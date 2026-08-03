//! Deterministic frame-level fault injection for server wire tests.
//!
//! The harness deliberately has no clock. A pause returns a token and advances
//! only when the test calls [`FaultScript::resume`]; saturation and lag are
//! explicit outcomes rather than scheduler-dependent channel races.

use std::fmt::{self, Debug, Display};

use bytes::{Bytes, BytesMut};
use phux_protocol::wire::frame::FrameKind;

/// A stable point at which a frame can be intercepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Milestone {
    BootstrapBegin,
    CaptureRecord(u32),
    BootstrapReady,
    LiveOutput(u64),
    HistoryPage(u64),
    BootstrapTombstone,
    HistoryTombstone,
    Reconnect,
}

/// A deterministic transport/server pressure fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Pause,
    Drop,
    Duplicate,
    CorruptPayload,
    Disconnect,
    BroadcastLag,
    MailboxSaturation,
}

/// One single-use fault scheduled at a wire milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultStep {
    pub at: Milestone,
    pub fault: Fault,
}

impl FaultStep {
    #[must_use]
    pub const fn new(at: Milestone, fault: Fault) -> Self {
        Self { at, fault }
    }
}

/// Identity of a deterministically paused frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PauseToken(u64);

/// Result of passing one frame through a [`FaultScript`].
#[derive(Debug, Clone, PartialEq)]
pub enum FaultOutcome {
    /// One normal frame or two byte-identical frames for `Duplicate`.
    Delivered(Vec<FrameKind>),
    /// The frame is retained by the script until explicitly resumed.
    Paused(PauseToken),
    /// No frame reached this client.
    Dropped,
    /// This client's transport was severed before the frame was delivered.
    Disconnected,
    /// Broadcast continuity was lost. The server must tombstone/resync.
    BroadcastLag,
    /// This client's bounded outbound mailbox rejected the frame. Peers are
    /// unaffected; the server must tombstone/resync this client.
    MailboxSaturation,
    /// The requested mutation did not apply to a payload-bearing frame.
    Diagnostic(String),
}

#[derive(Debug, Clone)]
struct PausedFrame {
    token: PauseToken,
    at: Milestone,
    frame: FrameKind,
}

/// An ordered, single-use fault program.
///
/// Steps need not be sorted. The first unconsumed step matching a milestone is
/// applied, which makes tables concise while preserving repeated milestones.
#[derive(Debug, Clone)]
pub struct FaultScript {
    steps: Vec<Option<FaultStep>>,
    paused: Vec<PausedFrame>,
    next_pause: u64,
}

impl FaultScript {
    #[must_use]
    pub fn new(steps: impl IntoIterator<Item = FaultStep>) -> Self {
        Self {
            steps: steps.into_iter().map(Some).collect(),
            paused: Vec::new(),
            next_pause: 1,
        }
    }

    #[must_use]
    pub fn clean() -> Self {
        Self::new([])
    }

    /// Intercept one server-to-client frame at `at`.
    pub fn transmit(
        &mut self,
        at: Milestone,
        frame: FrameKind,
        transcript: &mut WireTranscript,
    ) -> FaultOutcome {
        let fault = self
            .steps
            .iter_mut()
            .find(|step| step.as_ref().is_some_and(|step| step.at == at))
            .and_then(Option::take)
            .map(|step| step.fault);

        let outcome = match fault {
            None => FaultOutcome::Delivered(vec![wire_round_trip(&frame)]),
            Some(Fault::Pause) => {
                let token = PauseToken(self.next_pause);
                self.next_pause = self
                    .next_pause
                    .checked_add(1)
                    .expect("pause token overflow");
                self.paused.push(PausedFrame { token, at, frame });
                FaultOutcome::Paused(token)
            }
            Some(Fault::Drop) => FaultOutcome::Dropped,
            Some(Fault::Duplicate) => {
                let frame = wire_round_trip(&frame);
                FaultOutcome::Delivered(vec![frame.clone(), frame])
            }
            Some(Fault::CorruptPayload) => match corrupt_payload(frame) {
                Ok(frame) => FaultOutcome::Delivered(vec![wire_round_trip(&frame)]),
                Err(message) => FaultOutcome::Diagnostic(message),
            },
            Some(Fault::Disconnect) => FaultOutcome::Disconnected,
            Some(Fault::BroadcastLag) => FaultOutcome::BroadcastLag,
            Some(Fault::MailboxSaturation) => FaultOutcome::MailboxSaturation,
        };
        transcript.push(at, fault, &outcome);
        outcome
    }

    /// Release exactly one paused frame. Resumption itself cannot re-trigger
    /// the fault at that milestone.
    pub fn resume(
        &mut self,
        token: PauseToken,
        transcript: &mut WireTranscript,
    ) -> Result<FrameKind, String> {
        let Some(index) = self.paused.iter().position(|paused| paused.token == token) else {
            let message = format!("unknown or already-resumed pause token {token:?}");
            transcript.note(message.clone());
            return Err(message);
        };
        let paused = self.paused.remove(index);
        let frame = wire_round_trip(&paused.frame);
        transcript.note(format!("resume {:?} -> {}", paused.at, frame_label(&frame)));
        Ok(frame)
    }

    /// Require every scheduled fault and pause to have been consumed.
    pub fn assert_drained(&self, transcript: &WireTranscript) {
        transcript.assert(
            self.steps.iter().all(Option::is_none) && self.paused.is_empty(),
            format_args!(
                "fault script not drained: remaining_steps={:?}, paused={:?}",
                self.steps, self.paused
            ),
        );
    }
}

/// One diagnostic event in a [`WireTranscript`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEvent {
    pub ordinal: usize,
    pub detail: String,
}

/// Ordered diagnostic trace included in every harness assertion failure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WireTranscript {
    events: Vec<TranscriptEvent>,
}

impl WireTranscript {
    #[must_use]
    pub const fn new() -> Self {
        Self { events: Vec::new() }
    }

    pub fn note(&mut self, detail: impl Into<String>) {
        let ordinal = self.events.len();
        self.events.push(TranscriptEvent {
            ordinal,
            detail: detail.into(),
        });
    }

    fn push(&mut self, at: Milestone, fault: Option<Fault>, outcome: &FaultOutcome) {
        self.note(format!(
            "{at:?} fault={fault:?} outcome={}",
            outcome_label(outcome)
        ));
    }

    #[must_use]
    pub fn events(&self) -> &[TranscriptEvent] {
        &self.events
    }

    pub fn assert(&self, condition: bool, message: fmt::Arguments<'_>) {
        assert!(condition, "{message}\nwire transcript:\n{self}");
    }

    pub fn assert_eq<T: Debug + PartialEq>(&self, actual: &T, expected: &T, label: &str) {
        assert_eq!(actual, expected, "{label}\nwire transcript:\n{self}");
    }

    pub fn fail(&self, message: impl Display) -> ! {
        panic!("{message}\nwire transcript:\n{self}")
    }
}

impl Display for WireTranscript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.events.is_empty() {
            return f.write_str("  <empty>");
        }
        for event in &self.events {
            writeln!(f, "  {:03}: {}", event.ordinal, event.detail)?;
        }
        Ok(())
    }
}

fn wire_round_trip(frame: &FrameKind) -> FrameKind {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let (decoded, rest) =
        FrameKind::decode(&encoded).expect("harness generated invalid wire frame");
    assert!(rest.is_empty(), "harness decoder left trailing bytes");
    decoded
}

fn corrupt_payload(frame: FrameKind) -> Result<FrameKind, String> {
    fn flipped(bytes: &Bytes) -> Result<Bytes, String> {
        if bytes.is_empty() {
            return Err("cannot corrupt an empty opaque payload".to_owned());
        }
        let mut bytes = bytes.to_vec();
        let index = bytes.len() / 2;
        bytes[index] ^= 0x80;
        Ok(Bytes::from(bytes))
    }

    match frame {
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => Ok(FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload: flipped(&payload)?,
        }),
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            cursor,
            next_cursor,
            payload,
            rows,
        } => Ok(FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            cursor,
            next_cursor,
            payload: flipped(&payload)?,
            rows,
        }),
        FrameKind::TerminalOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => Ok(FrameKind::TerminalOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes: flipped(&bytes)?,
        }),
        other => Err(format!(
            "CorruptPayload requires BOOTSTRAP_CHUNK, HISTORY_PAGE, or TERMINAL_OUTPUT; got {}",
            frame_label(&other)
        )),
    }
}

const fn frame_label(frame: &FrameKind) -> &'static str {
    match frame {
        FrameKind::BootstrapBegin { .. } => "BOOTSTRAP_BEGIN",
        FrameKind::BootstrapChunk { .. } => "BOOTSTRAP_CHUNK",
        FrameKind::BootstrapReady { .. } => "BOOTSTRAP_READY",
        FrameKind::TerminalOutput { .. } => "TERMINAL_OUTPUT",
        FrameKind::HistoryPage { .. } => "HISTORY_PAGE",
        FrameKind::BootstrapTombstone { .. } => "BOOTSTRAP_TOMBSTONE",
        FrameKind::HistoryTombstone { .. } => "HISTORY_TOMBSTONE",
        _ => "OTHER",
    }
}

fn outcome_label(outcome: &FaultOutcome) -> String {
    match outcome {
        FaultOutcome::Delivered(frames) => format!(
            "delivered[{}]",
            frames.iter().map(frame_label).collect::<Vec<_>>().join(",")
        ),
        FaultOutcome::Paused(token) => format!("paused({token:?})"),
        FaultOutcome::Dropped => "dropped".to_owned(),
        FaultOutcome::Disconnected => "disconnected".to_owned(),
        FaultOutcome::BroadcastLag => "broadcast-lag".to_owned(),
        FaultOutcome::MailboxSaturation => "mailbox-saturation".to_owned(),
        FaultOutcome::Diagnostic(message) => format!("diagnostic({message})"),
    }
}
