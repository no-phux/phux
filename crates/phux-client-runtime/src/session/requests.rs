//! Correlated requests: send one request frame, then read until the peer
//! answers its `request_id`, keeping every other frame for the caller.

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, MoveResult, Scope, SpawnResult,
};

use super::reply::refusal_for;
use super::{Answer, Refusal, Reply, Session, SessionError};

/// How many `GET_METADATA`s [`Session::request_metadata_many`] keeps in
/// flight. 64 requests of about 40 bytes stay well inside the smallest
/// socket buffer a platform ships (8 KiB on macOS).
pub const METADATA_WINDOW: usize = 64;

/// One metadata value: the bytes, or `None` for an unset key.
type MetadataValue = Option<Vec<u8>>;

impl Session {
    /// Send one `COMMAND` and wait for its reply, keeping every frame the
    /// server interleaved ahead of it (see the interleave contract on
    /// [`super`]).
    ///
    /// The reply is a `COMMAND_RESULT` for `request_id`, or an `ERROR`
    /// correlated to it (proto.md §9), folded into [`CommandResult::Error`];
    /// without that a peer refusing an unimplemented command would hang the
    /// caller. An uncorrelated `ERROR` (a hub's satellite degradation
    /// notice) is not an answer and lands in [`Reply::interleaved`].
    ///
    /// # Errors
    ///
    /// Transport and decode failures; a server that closes without replying
    /// is [`SessionError::Disconnected`]. A native SDK command without
    /// negotiated `NATIVE_AGENT` support is refused before writing any bytes.
    pub async fn request(
        &mut self,
        request_id: u32,
        command: Command,
    ) -> Result<Reply, SessionError> {
        if matches!(&command, Command::NativeAgent { .. })
            && !self.negotiated_bootstrap().is_some_and(|negotiated| {
                negotiated
                    .server_features_ext
                    .contains(ServerFeatureExt::NativeAgent)
            })
        {
            return Err(SessionError::Protocol(
                "Native SDK sessions require NATIVE_AGENT support; upgrade the executing host."
                    .into(),
            ));
        }
        let frame = FrameKind::Command {
            request_id,
            command,
        };
        let (answer, interleaved) = self
            .round_trip(request_id, &frame, |frame| match frame {
                FrameKind::CommandResult { request_id, result } => {
                    Some((*request_id, result.clone()))
                }
                _ => None,
            })
            .await?
            .into_parts();
        // `CommandResult::Error` carries exactly the `ERROR` payload.
        let result = answer.unwrap_or_else(|refusal| CommandResult::Error {
            code: refusal.code,
            message: refusal.message,
        });
        Ok(Reply::new(result, interleaved))
    }

    /// Send one `GET_METADATA` and wait for its `METADATA_VALUE`, keeping
    /// interleaved frames. A refusal is an `Err` answer, distinct from an
    /// unset key (`Ok(None)`).
    ///
    /// # Errors
    ///
    /// Transport and decode failures.
    pub async fn request_metadata(
        &mut self,
        request_id: u32,
        scope: Scope,
        key: String,
    ) -> Result<Reply<Answer<MetadataValue>>, SessionError> {
        let frame = FrameKind::GetMetadata {
            request_id,
            scope,
            key,
        };
        self.round_trip(request_id, &frame, |frame| match frame {
            FrameKind::MetadataValue { request_id, value } => Some((*request_id, value.clone())),
            _ => None,
        })
        .await
    }

    /// One `GET_METADATA` per `(scope, key)`, pipelined: answers come back
    /// in `reads` order, a refusal as an `Err` answer, interleaved frames
    /// kept. Request ids run from `first_request_id`.
    ///
    /// Reads go out [`METADATA_WINDOW`] at a time, each window in one write,
    /// so `n` reads cost `n / METADATA_WINDOW` round trips instead of `n`.
    /// The window stays small enough that its requests always fit the
    /// socket buffer: a server that stops reading while its replies queue
    /// cannot block this write and deadlock the pair.
    ///
    /// # Errors
    ///
    /// Transport and decode failures; a server that closes before answering
    /// every read is [`SessionError::Disconnected`].
    pub async fn request_metadata_many(
        &mut self,
        first_request_id: u32,
        reads: Vec<(Scope, String)>,
    ) -> Result<Reply<Vec<Answer<MetadataValue>>>, SessionError> {
        let mut batch = MetadataBatch::new(first_request_id, reads.len());
        let mut reads = reads.into_iter();
        while let Some(window) = batch.next_window() {
            let requests = window.clone().zip(reads.by_ref());
            self.send_metadata_window(first_request_id, requests)
                .await?;
            while batch.awaiting() {
                let frame = self.recv().await?;
                batch.accept(frame, &window);
            }
        }
        Ok(batch.finish())
    }

    /// Send one window of reads in one write. The cork is lifted on every
    /// path, and the first send failure wins over the batch's.
    async fn send_metadata_window(
        &mut self,
        first_request_id: u32,
        requests: impl Iterator<Item = (usize, (Scope, String))>,
    ) -> Result<(), SessionError> {
        self.cork();
        let mut sent = Ok(());
        for (index, (scope, key)) in requests {
            let frame = FrameKind::GetMetadata {
                request_id: request_id_at(first_request_id, index),
                scope,
                key,
            };
            sent = self.send(&frame).await;
            if sent.is_err() {
                break;
            }
        }
        let shipped = self.uncork().await;
        sent.and(shipped)
    }

    /// Send one `LIST_METADATA` and wait for its `METADATA_KEYS`, keeping
    /// interleaved frames.
    ///
    /// # Errors
    ///
    /// Transport and decode failures.
    pub async fn request_metadata_keys(
        &mut self,
        request_id: u32,
        scope: Scope,
    ) -> Result<Reply<Answer<Vec<String>>>, SessionError> {
        let frame = FrameKind::ListMetadata { request_id, scope };
        self.round_trip(request_id, &frame, |frame| match frame {
            FrameKind::MetadataKeys { request_id, keys } => Some((*request_id, keys.clone())),
            _ => None,
        })
        .await
    }

    /// Send one `SPAWN_RESOURCE` and wait for its `RESOURCE_SPAWNED`,
    /// keeping interleaved frames. The correlation id is read from `frame`.
    /// A satellite may answer a relayed spawn with a correlated `ERROR`,
    /// which becomes an `Err` answer.
    ///
    /// # Errors
    ///
    /// [`SessionError::Protocol`] when `frame` is not a `SPAWN_RESOURCE`;
    /// otherwise transport and decode failures.
    pub async fn request_spawn(
        &mut self,
        frame: &FrameKind,
    ) -> Result<Reply<Answer<SpawnResult>>, SessionError> {
        let FrameKind::SpawnResource { request_id, .. } = frame else {
            return Err(SessionError::Protocol(format!(
                "request_spawn needs a SPAWN_RESOURCE frame, got {frame:?}",
            )));
        };
        self.round_trip(*request_id, frame, |frame| match frame {
            FrameKind::ResourceSpawned { request_id, result } => {
                Some((*request_id, result.clone()))
            }
            _ => None,
        })
        .await
    }

    /// Send one `MOVE_RESOURCE` and wait for its `RESOURCE_MOVED`, keeping
    /// interleaved frames (ADR-0056). The correlation id is read from
    /// `frame`.
    ///
    /// # Errors
    ///
    /// [`SessionError::Protocol`] when `frame` is not a `MOVE_RESOURCE`;
    /// otherwise transport and decode failures.
    pub async fn request_move(
        &mut self,
        frame: &FrameKind,
    ) -> Result<Reply<Answer<MoveResult>>, SessionError> {
        let FrameKind::MoveResource { request_id, .. } = frame else {
            return Err(SessionError::Protocol(format!(
                "request_move needs a MOVE_RESOURCE frame, got {frame:?}",
            )));
        };
        self.round_trip(*request_id, frame, |frame| match frame {
            FrameKind::ResourceMoved { request_id, result } => Some((*request_id, result.clone())),
            _ => None,
        })
        .await
    }

    /// Send `frame`, then read until the peer answers `request_id`.
    ///
    /// The one correlation loop: `recognize` names one pair's reply frame;
    /// the correlated-`ERROR` arm lives here so no pair can forget it. It
    /// borrows each frame and clones the payload out, so an unrecognized
    /// frame is kept whole.
    async fn round_trip<T>(
        &mut self,
        request_id: u32,
        frame: &FrameKind,
        recognize: impl Fn(&FrameKind) -> Option<(u32, T)>,
    ) -> Result<Reply<Answer<T>>, SessionError> {
        self.send(frame).await?;
        let mut interleaved = Vec::new();
        loop {
            let frame = self.recv().await?;
            if let Some(answer) = answer_to(&frame, request_id, &recognize) {
                return Ok(Reply::new(answer, interleaved));
            }
            interleaved.push(frame);
        }
    }
}

/// `frame`'s answer to `request_id`, if it is one: the recognized reply, or
/// a correlated refusal.
fn answer_to<T>(
    frame: &FrameKind,
    request_id: u32,
    recognize: impl Fn(&FrameKind) -> Option<(u32, T)>,
) -> Option<Answer<T>> {
    if let Some((got, value)) = recognize(frame)
        && got == request_id
    {
        return Some(Ok(value));
    }
    refusal_for(frame, request_id).map(Err)
}

/// The request id of the `index`th read in a batch numbered from `first`.
fn request_id_at(first: u32, index: usize) -> u32 {
    first.wrapping_add(u32::try_from(index).unwrap_or(u32::MAX))
}

/// A pipelined metadata read's progress, one window at a time.
struct MetadataBatch {
    first_request_id: u32,
    answers: Vec<Option<Answer<MetadataValue>>>,
    interleaved: Vec<FrameKind>,
    /// Where the next window starts.
    next: usize,
    /// Reads of the current window still unanswered.
    pending: usize,
}

impl MetadataBatch {
    fn new(first_request_id: u32, reads: usize) -> Self {
        Self {
            first_request_id,
            answers: vec![None; reads],
            interleaved: Vec::new(),
            next: 0,
            pending: 0,
        }
    }

    /// The next window of read indices, or `None` when every read is out.
    fn next_window(&mut self) -> Option<std::ops::Range<usize>> {
        if self.next >= self.answers.len() {
            return None;
        }
        let window = self.next..self.answers.len().min(self.next + METADATA_WINDOW);
        self.next = window.end;
        self.pending = window.len();
        Some(window)
    }

    /// Whether the current window still has unanswered reads.
    const fn awaiting(&self) -> bool {
        self.pending > 0
    }

    /// File `frame` as the answer to a read in `window`, or keep it as an
    /// interleaved frame. A read answers once; a repeat is interleaved.
    fn accept(&mut self, frame: FrameKind, window: &std::ops::Range<usize>) {
        let Some(index) = self.unanswered_read(&frame, window) else {
            self.interleaved.push(frame);
            return;
        };
        self.answers[index] = Some(match frame {
            FrameKind::MetadataValue { value, .. } => Ok(value),
            FrameKind::Error { code, message, .. } => Err(Refusal { code, message }),
            other => {
                self.interleaved.push(other);
                return;
            }
        });
        self.pending -= 1;
    }

    /// The unanswered read in `window` that `frame` answers, if any
    /// (proto.md §9: an `ERROR` carrying a read's id answers it).
    fn unanswered_read(&self, frame: &FrameKind, window: &std::ops::Range<usize>) -> Option<usize> {
        let request_id = match frame {
            FrameKind::MetadataValue { request_id, .. }
            | FrameKind::Error {
                request_id: Some(request_id),
                ..
            } => *request_id,
            _ => return None,
        };
        usize::try_from(request_id.wrapping_sub(self.first_request_id))
            .ok()
            .filter(|index| window.contains(index) && self.answers[*index].is_none())
    }

    fn finish(self) -> Reply<Vec<Answer<MetadataValue>>> {
        let answers = self
            .answers
            .into_iter()
            .map(|answer| answer.unwrap_or(Ok(None)))
            .collect();
        Reply::new(answers, self.interleaved)
    }
}
