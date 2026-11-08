//! Bounded stock encoding and transport-unit indexing, not a terminal decoder.
//!
//! Only trusted engine output is indexed here. The engine remains responsible
//! for snapshot semantics and CRC validation when the unchanged bytes are read.

use std::{io, io::Write, ops::Range};

use bytes::Bytes;
use libghostty_vt::Terminal;
use thiserror::Error;

const ENVELOPE: &[u8] = b"GHOSTSNP\x01\x00";
const HEADER_BYTES: usize = 10;
const PAGE: u16 = 3;
const HISTORY: u16 = 4;
const READY: u16 = 5;
const FINISH: u16 = 6;
const IMAGE: u16 = 9;
const PLACEMENTS: u16 = 10;

/// Independent retained-output and transport-unit bounds.
#[derive(Clone, Copy, Debug)]
#[allow(
    clippy::struct_field_names,
    reason = "explicit maximums are the shared capture contract"
)]
pub struct SnapshotCaptureLimits {
    /// Aggregate encoded bytes, enforced while the engine writes.
    pub max_bytes: usize,
    /// Maximum envelope, record, or grouped history unit size.
    pub max_record_bytes: usize,
    /// Maximum actual records (excluding the envelope).
    pub max_records: usize,
    /// Maximum rows declared by one history PAGE.
    pub max_rows: usize,
}

/// A history transport unit sharing storage with its snapshot.
#[derive(Clone, Debug)]
pub struct SnapshotHistoryRecord {
    /// Complete manifests/progress/optional-suffix bytes for this unit.
    pub range: Range<usize>,
    /// Rows in a PAGE, or zero for image pixels and FINISH.
    pub rows: usize,
    /// Only the final FINISH unit is terminal progress.
    pub finish: bool,
}

/// An immutable stock snapshot with zero-copy transport-unit ranges.
#[derive(Debug)]
pub struct EncodedSnapshot {
    /// Unmodified stock encoder output.
    pub bytes: Bytes,
    /// Envelope and individual records through READY, in order.
    pub prefix_records: Vec<Range<usize>>,
    /// Complete staged-reader units after READY, including final FINISH.
    pub history: Vec<SnapshotHistoryRecord>,
}

/// Failure before an encoded snapshot can be published.
#[derive(Debug, Error)]
pub enum SnapshotCaptureError {
    /// A retained-output or transport-unit budget was exceeded.
    #[error("snapshot capture limit exceeded")]
    LimitExceeded,
    /// A fallible retained-storage allocation failed.
    #[error("snapshot capture allocation failed")]
    OutOfMemory,
    /// Stock output had incomplete or unsupported framing.
    #[error("invalid snapshot framing")]
    InvalidSnapshot,
    /// The stock engine encoder failed.
    #[error(transparent)]
    Engine(#[from] libghostty_vt::Error),
}

type Result<T> = std::result::Result<T, SnapshotCaptureError>;

impl EncodedSnapshot {
    /// Encode once with an aggregate-bounded sink, then index staged units.
    ///
    /// # Errors
    /// Returns an engine, allocation, framing, or budget error before publication.
    pub fn capture(terminal: &Terminal<'_, '_>, limits: SnapshotCaptureLimits) -> Result<Self> {
        let mut sink = BoundedWriter {
            bytes: Vec::new(),
            max_bytes: limits.max_bytes,
            error: None,
        };
        let encoded = terminal.encode_snapshot(&mut sink);
        if let Some(error) = sink.error {
            return Err(error);
        }
        encoded?;
        Self::index(Bytes::from(sink.bytes), limits)
    }

    fn index(bytes: Bytes, limits: SnapshotCaptureLimits) -> Result<Self> {
        if bytes.len() > limits.max_bytes || HEADER_BYTES > limits.max_record_bytes {
            return Err(SnapshotCaptureError::LimitExceeded);
        }
        if !bytes.starts_with(ENVELOPE) {
            return Err(SnapshotCaptureError::InvalidSnapshot);
        }
        let mut snapshot = Self {
            bytes,
            prefix_records: Vec::new(),
            history: Vec::new(),
        };
        push(&mut snapshot.prefix_records, 0..ENVELOPE.len())?;
        let mut cursor = RecordCursor {
            bytes: &snapshot.bytes,
            offset: ENVELOPE.len(),
            count: 0,
            limits,
        };
        cursor.prefix(&mut snapshot.prefix_records)?;
        loop {
            let unit = cursor.history_unit()?;
            let finish = unit.finish;
            push(&mut snapshot.history, unit)?;
            if finish {
                break;
            }
        }
        Ok(snapshot)
    }
}

fn push<T>(items: &mut Vec<T>, item: T) -> Result<()> {
    items
        .try_reserve(1)
        .map_err(|_| SnapshotCaptureError::OutOfMemory)?;
    items.push(item);
    Ok(())
}

struct BoundedWriter {
    bytes: Vec<u8>,
    max_bytes: usize,
    error: Option<SnapshotCaptureError>,
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.error.is_some() {
            return Err(io::Error::other("snapshot sink already failed"));
        }
        let result = self.append(bytes);
        if let Err(error) = result {
            self.error = Some(error);
            return Err(io::Error::other("snapshot sink rejected output"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl BoundedWriter {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(SnapshotCaptureError::LimitExceeded)?;
        if end > self.max_bytes {
            return Err(SnapshotCaptureError::LimitExceeded);
        }
        if end > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(end)
                .min(self.max_bytes);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(|_| SnapshotCaptureError::OutOfMemory)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

struct Record {
    tag: u16,
    range: Range<usize>,
}

impl Record {
    fn require_empty(&self) -> Result<()> {
        if self.range.len() != HEADER_BYTES {
            return Err(SnapshotCaptureError::InvalidSnapshot);
        }
        Ok(())
    }
}

struct RecordCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
    count: usize,
    limits: SnapshotCaptureLimits,
}

impl RecordCursor<'_> {
    fn prefix(&mut self, records: &mut Vec<Range<usize>>) -> Result<()> {
        loop {
            let record = self.take()?;
            push(records, record.range.clone())?;
            if record.tag == READY {
                return record.require_empty();
            }
            if record.tag == FINISH {
                return Err(SnapshotCaptureError::InvalidSnapshot);
            }
        }
    }

    fn peek(&self, offset: usize) -> Result<Record> {
        let header_end = offset
            .checked_add(HEADER_BYTES)
            .ok_or(SnapshotCaptureError::InvalidSnapshot)?;
        let header = self
            .bytes
            .get(offset..header_end)
            .ok_or(SnapshotCaptureError::InvalidSnapshot)?;
        let tag = u16::from_le_bytes([header[0], header[1]]);
        let len = u32::from_le_bytes([header[2], header[3], header[4], header[5]]) as usize;
        let end = header_end
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(SnapshotCaptureError::InvalidSnapshot)?;
        if end - offset > self.limits.max_record_bytes {
            return Err(SnapshotCaptureError::LimitExceeded);
        }
        Ok(Record {
            tag,
            range: offset..end,
        })
    }

    fn take(&mut self) -> Result<Record> {
        if self.count >= self.limits.max_records {
            return Err(SnapshotCaptureError::LimitExceeded);
        }
        let record = self.peek(self.offset)?;
        self.offset = record.range.end;
        self.count += 1;
        Ok(record)
    }

    fn history_unit(&mut self) -> Result<SnapshotHistoryRecord> {
        let start = self.offset;
        let mut manifests = 0;
        let record = loop {
            let record = self.take()?;
            match record.tag {
                HISTORY if manifests < 2 => manifests += 1,
                0 | 11..=u16::MAX => {}
                _ => break record,
            }
        };
        let rows = self.progress(&record)?;
        if self.offset - start > self.limits.max_record_bytes {
            return Err(SnapshotCaptureError::LimitExceeded);
        }
        Ok(SnapshotHistoryRecord {
            range: start..self.offset,
            rows,
            finish: record.tag == FINISH,
        })
    }

    fn progress(&mut self, record: &Record) -> Result<usize> {
        match record.tag {
            PAGE => {
                let payload = &self.bytes[record.range.start + HEADER_BYTES..record.range.end];
                let header = payload
                    .get(..4)
                    .ok_or(SnapshotCaptureError::InvalidSnapshot)?;
                let rows = usize::from(u16::from_le_bytes([header[2], header[3]]));
                if rows == 0 {
                    return Err(SnapshotCaptureError::InvalidSnapshot);
                }
                if rows > self.limits.max_rows {
                    return Err(SnapshotCaptureError::LimitExceeded);
                }
                self.page_suffix()?;
                Ok(rows)
            }
            IMAGE => Ok(0),
            FINISH => {
                record.require_empty()?;
                if self.offset != self.bytes.len() {
                    return Err(SnapshotCaptureError::InvalidSnapshot);
                }
                Ok(0)
            }
            _ => Err(SnapshotCaptureError::InvalidSnapshot),
        }
    }

    fn page_suffix(&mut self) -> Result<()> {
        let mut end = self.offset;
        while end < self.bytes.len() {
            let record = self.peek(end)?;
            end = record.range.end;
            match record.tag {
                PLACEMENTS => {
                    while self.offset < end {
                        self.take()?;
                    }
                    return Ok(());
                }
                0 | 11..=u16::MAX => {}
                _ => break,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
