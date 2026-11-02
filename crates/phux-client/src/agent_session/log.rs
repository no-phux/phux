//! Reading an agent session's log: retained records, then live follow.

use super::{
    AgentEvent, AgentEventRecord, AgentSessionError, AttachError, Command, CommandResult,
    Connection, ErrorCode, FrameKind, LogEnd, LogOptions, LogOutcome, MAX_RECORD_BYTES,
    REQUEST_ATTACH, REQUEST_DETACH, ResourceId, map_refusal, require_support,
};

/// Read `resource`'s stream as an observer.
///
/// The retained records (trimmed to `options.tail`) arrive after `READY`;
/// with `follow`, live records follow until the session closes, the server
/// goes away, or `sink` returns `false`.
///
/// # Errors
///
/// [`AgentSessionError::Unsupported`], the server's refusal of the attach
/// (`WRONG_RESOURCE_KIND` for a Terminal target), a malformed record on the
/// stream, or transport failure.
pub async fn log(
    conn: &mut Connection,
    resource: &ResourceId,
    options: LogOptions,
    mut sink: impl FnMut(AgentEventRecord) -> bool,
) -> Result<LogOutcome, AgentSessionError> {
    require_support(conn)?;
    let (result, primed) = conn
        .request(
            REQUEST_ATTACH,
            // A log follower only reads (ADR-0127).
            Command::attach_agent_session(resource.clone(), conn.observer_role_policy(), None),
        )
        .await?
        .into_parts();
    if let CommandResult::Error { code, message } = result {
        return Err(map_refusal(resource, code, message));
    }
    // Once the attach is accepted, every read/bind/subscription failure must
    // release the observer pump as well as its local transport binding.
    let outcome = read_log(conn, resource, options, primed.into_iter(), &mut sink).await;
    detach(conn, resource).await;
    outcome
}

pub(super) async fn read_log(
    conn: &mut Connection,
    resource: &ResourceId,
    options: LogOptions,
    mut pending: std::vec::IntoIter<FrameKind>,
    sink: &mut impl FnMut(AgentEventRecord) -> bool,
) -> Result<LogOutcome, AgentSessionError> {
    conn.bind_terminal(resource).await?;
    if options.follow {
        conn.send(&FrameKind::SubscribeEvents {
            terminal: Some(resource.clone()),
            after_seq: None,
        })
        .await?;
    }

    // Retained records: everything up to BOOTSTRAP_READY, usually all in
    // `primed` (the server pushes the transcript ahead of the ack).
    let mut reader = Reader::new(resource.clone());
    let (retained, closed) = read_retained(conn, &mut reader, &mut pending).await?;
    let retained_count = retained.len();
    let skip = options
        .tail
        .map_or(0, |tail| retained_count.saturating_sub(tail));
    let mut delivered = 0;
    let end = if !deliver(retained.into_iter().skip(skip), sink, &mut delivered) {
        LogEnd::Stopped
    } else if closed {
        LogEnd::SessionClosed
    } else if options.follow {
        follow(conn, &mut reader, pending, sink, &mut delivered).await?
    } else {
        LogEnd::Retained
    };
    Ok(LogOutcome {
        retained: retained_count,
        delivered,
        end,
    })
}

/// Stage retained output until the server proves the transcript complete.
pub(super) async fn read_retained(
    conn: &mut Connection,
    reader: &mut Reader,
    pending: &mut std::vec::IntoIter<FrameKind>,
) -> Result<(Vec<AgentEventRecord>, bool), AgentSessionError> {
    let mut retained = Vec::new();
    loop {
        let frame = match pending.next() {
            Some(frame) => frame,
            None => conn.recv().await?,
        };
        match reader.absorb(frame)? {
            Absorbed::Records(records) => retained.extend(records),
            // A session closed mid-bootstrap still answers with complete lines.
            Absorbed::Ready => return Ok((retained, false)),
            Absorbed::Closed => return Ok((retained, true)),
            Absorbed::Gap {
                after_seq,
                next_seq,
            } => {
                return Err(reader.protocol_error(format!(
                    "retained records jump from seq {after_seq} to {next_seq}"
                )));
            }
            Absorbed::Nothing => {}
        }
    }
}

/// Hand `records` to `sink`, counting each; `false` once the sink stops.
pub(super) fn deliver(
    records: impl IntoIterator<Item = AgentEventRecord>,
    sink: &mut impl FnMut(AgentEventRecord) -> bool,
    delivered: &mut usize,
) -> bool {
    for record in records {
        *delivered += 1;
        if !sink(record) {
            return false;
        }
    }
    true
}

/// Live records until the session closes or the stream ends; frames the
/// attach interleaved after READY are drained first.
pub(super) async fn follow(
    conn: &mut Connection,
    reader: &mut Reader,
    mut pending: std::vec::IntoIter<FrameKind>,
    sink: &mut impl FnMut(AgentEventRecord) -> bool,
    delivered: &mut usize,
) -> Result<LogEnd, AgentSessionError> {
    Ok(loop {
        let frame = match pending.next() {
            Some(frame) => Ok(frame),
            None => conn.recv().await,
        };
        let frame = match frame {
            Ok(frame) => frame,
            Err(AttachError::Disconnected) => break LogEnd::Disconnected,
            Err(err) => return Err(err.into()),
        };
        match reader.absorb(frame)? {
            Absorbed::Records(records) => {
                if !deliver(records, sink, delivered) {
                    break LogEnd::Stopped;
                }
            }
            Absorbed::Closed => break LogEnd::SessionClosed,
            Absorbed::Gap {
                after_seq,
                next_seq,
            } => {
                break LogEnd::RecordsLost {
                    after_seq,
                    next_seq,
                };
            }
            Absorbed::Ready | Absorbed::Nothing => {}
        }
    })
}

/// Best-effort `DETACH_RESOURCE` (idempotent, L1 §5.1): never turns a
/// completed read into a failure.
pub(super) async fn detach(conn: &mut Connection, resource: &ResourceId) {
    let _ = conn
        .send(&FrameKind::Command {
            request_id: REQUEST_DETACH,
            command: Command::DetachResource {
                terminal_id: resource.clone(),
            },
        })
        .await;
    conn.unbind_terminal(resource);
}

/// What one absorbed frame contributed.
pub(super) enum Absorbed {
    /// Complete records decoded from a bootstrap chunk or a live frame.
    Records(Vec<AgentEventRecord>),
    /// `BOOTSTRAP_READY`: the retained records are complete.
    Ready,
    /// The session closed.
    Closed,
    /// The stream resumed at `next_seq` after evicting records this read
    /// never saw; nothing past `after_seq` was delivered.
    Gap {
        /// The last record this read holds.
        after_seq: u64,
        /// The first record the stream still has.
        next_seq: u64,
    },
    /// A frame that carries nothing for this read.
    Nothing,
}

/// The frame-to-record projection, with the line buffer that lets a record
/// straddle two bootstrap chunks.
pub(super) struct Reader {
    resource: ResourceId,
    pub(super) buffer: Vec<u8>,
    /// The last record this read has absorbed.
    last_seq: Option<u64>,
}

impl Reader {
    pub(super) const fn new(resource: ResourceId) -> Self {
        Self {
            resource,
            buffer: Vec::new(),
            last_seq: None,
        }
    }

    /// Drop records this read already holds and refuse to bridge a hole. A
    /// server replaces a lagged generation with its full retained ring, which
    /// either overlaps what was delivered or starts after evicted records.
    fn admit(&mut self, records: Vec<AgentEventRecord>) -> Absorbed {
        let fresh: Vec<_> = match self.last_seq {
            Some(last) => records.into_iter().filter(|r| r.seq > last).collect(),
            None => records,
        };
        if let (Some(after_seq), Some(first)) = (self.last_seq, fresh.first())
            && first.seq > after_seq.saturating_add(1)
        {
            return Absorbed::Gap {
                after_seq,
                next_seq: first.seq,
            };
        }
        if let Some(last) = fresh.last() {
            self.last_seq = Some(last.seq);
        }
        Absorbed::Records(fresh)
    }

    pub(super) fn absorb(&mut self, frame: FrameKind) -> Result<Absorbed, AgentSessionError> {
        match frame {
            bootstrap @ (FrameKind::BootstrapBegin { .. }
            | FrameKind::BootstrapChunk { .. }
            | FrameKind::BootstrapReady { .. }) => self.absorb_bootstrap(bootstrap),
            FrameKind::ResourceOutput {
                terminal_id, bytes, ..
            } if terminal_id == self.resource => {
                let records = self.decode_live(&bytes)?;
                Ok(self.admit(records))
            }
            FrameKind::ResourceClosed {
                terminal_id: terminal,
                ..
            }
            | FrameKind::Event {
                terminal: Some(terminal),
                event: AgentEvent::ResourceClosed { .. },
                ..
            } if terminal == self.resource => {
                self.finish()?;
                Ok(Absorbed::Closed)
            }
            FrameKind::Error {
                request_id: None,
                message,
                ..
            } => {
                crate::state::report_degradation(&[FrameKind::Error {
                    request_id: None,
                    code: ErrorCode::UnsupportedSatelliteRoute,
                    message,
                }]);
                Ok(Absorbed::Nothing)
            }
            FrameKind::Error { message, .. } => Err(AgentSessionError::Refused(message)),
            _ => Ok(Absorbed::Nothing),
        }
    }

    pub(super) fn absorb_bootstrap(
        &mut self,
        frame: FrameKind,
    ) -> Result<Absorbed, AgentSessionError> {
        match frame {
            FrameKind::BootstrapBegin { terminal_id, .. } if terminal_id == self.resource => {
                // A replacement generation drops the previous half-line.
                self.buffer.clear();
                Ok(Absorbed::Nothing)
            }
            FrameKind::BootstrapChunk {
                terminal_id,
                payload,
                ..
            } if terminal_id == self.resource => {
                let records = self.decode(&payload)?;
                Ok(self.admit(records))
            }
            FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == self.resource => {
                self.finish()?;
                Ok(Absorbed::Ready)
            }
            _ => Ok(Absorbed::Nothing),
        }
    }

    /// Append `bytes` to the line buffer and decode every complete line.
    pub(super) fn decode(
        &mut self,
        bytes: &[u8],
    ) -> Result<Vec<AgentEventRecord>, AgentSessionError> {
        let mut records = Vec::new();
        for fragment in bytes.split_inclusive(|byte| *byte == b'\n') {
            let content = fragment.strip_suffix(b"\n").unwrap_or(fragment);
            let wire_len = self
                .buffer
                .len()
                .saturating_add(content.len())
                .saturating_add(1);
            if wire_len > MAX_RECORD_BYTES {
                return Err(self.protocol_error(format!(
                    "record of {wire_len} bytes exceeds the {MAX_RECORD_BYTES}-byte codec limit"
                )));
            }
            self.buffer.extend_from_slice(content);
            if fragment.ends_with(b"\n") {
                if !self.buffer.iter().all(u8::is_ascii_whitespace) {
                    let record = serde_json::from_slice(&self.buffer)
                        .map_err(|error| self.protocol_error(error))?;
                    records.push(record);
                }
                self.buffer.clear();
            }
        }
        Ok(records)
    }

    /// Live output carries complete records, unlike bootstrap chunks.
    pub(super) fn decode_live(
        &mut self,
        bytes: &[u8],
    ) -> Result<Vec<AgentEventRecord>, AgentSessionError> {
        let records = self.decode(bytes)?;
        self.finish()?;
        if records.is_empty() {
            return Err(self.protocol_error("live output carried no records"));
        }
        Ok(records)
    }

    pub(super) fn finish(&self) -> Result<(), AgentSessionError> {
        if !self.buffer.is_empty() {
            return Err(self.protocol_error("incomplete JSONL record (missing LF)"));
        }
        Ok(())
    }

    pub(super) fn protocol_error(&self, reason: impl std::fmt::Display) -> AgentSessionError {
        AttachError::Protocol(format!(
            "malformed AgentEventsJsonlV1 record on {}: {reason}",
            crate::selector::format_terminal_id(&self.resource)
        ))
        .into()
    }
}
