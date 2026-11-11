//! Bounded GHOSTSNP transport-unit framing. Authentication stays in the engine.

#[cfg(test)]
mod tests;

// `{ tag: u16, payload_len: u32, crc32c: u32 }`.
const SNAPSHOT_RECORD_HEADER_BYTES: usize = 10;
const SNAPSHOT_RECORD_PAGE: u16 = 3;
const SNAPSHOT_RECORD_HISTORY: u16 = 4;
const SNAPSHOT_RECORD_FINISH: u16 = 6;
const SNAPSHOT_RECORD_IMAGE: u16 = 9;
const SNAPSHOT_RECORD_PAGE_PLACEMENTS: u16 = 10;

/// One record's `(tag, payload start, end)` at `offset`, if it fits `bytes`.
fn snapshot_record(bytes: &[u8], offset: usize) -> Option<(u16, usize, usize)> {
    let payload = offset.checked_add(SNAPSHOT_RECORD_HEADER_BYTES)?;
    let header = bytes.get(offset..payload)?;
    let tag = u16::from_le_bytes([header[0], header[1]]);
    let len = u32::from_le_bytes([header[2], header[3], header[4], header[5]]) as usize;
    let end = payload.checked_add(len).filter(|end| *end <= bytes.len())?;
    Some((tag, payload, end))
}

/// The end of the READY record: the split between renderable prefix and history.
pub(super) fn snapshot_ready_end(bytes: &[u8]) -> Option<usize> {
    const ENVELOPE_BYTES: usize = 10;
    const RECORD_READY: u16 = 5;
    let mut offset = ENVELOPE_BYTES;
    loop {
        let (tag, _, end) = snapshot_record(bytes, offset)?;
        if tag == RECORD_READY {
            return Some(end);
        }
        offset = end;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryUnit {
    Page { rows: usize, placements: bool },
    Image,
    Finish,
}

impl HistoryUnit {
    pub(super) fn rows(self) -> usize {
        match self {
            Self::Page { rows, .. } => rows,
            Self::Image | Self::Finish => 0,
        }
    }
}

/// A PAGE and its optional Kitty placements are one authenticated progress unit.
fn page_unit(bytes: &[u8], payload: usize, end: usize) -> Option<HistoryUnit> {
    let header_end = payload.checked_add(4)?;
    if header_end > end {
        return None;
    }
    let header = bytes.get(payload..header_end)?;
    let rows = u16::from_le_bytes([header[2], header[3]]);
    Some(HistoryUnit::Page {
        rows: usize::from(rows),
        placements: page_placements(bytes, end)?,
    })
}

fn unknown_snapshot_record(tag: u16) -> bool {
    tag == 0 || tag > SNAPSHOT_RECORD_PAGE_PLACEMENTS
}

/// Optional extension records may precede placements, but a second known
/// progress record must never be folded into a PAGE unit.
fn page_placements(bytes: &[u8], mut offset: usize) -> Option<bool> {
    if offset == bytes.len() {
        return Some(false);
    }
    loop {
        let (tag, _, end) = snapshot_record(bytes, offset)?;
        if tag == SNAPSHOT_RECORD_PAGE_PLACEMENTS && end == bytes.len() {
            return Some(true);
        }
        if !unknown_snapshot_record(tag) {
            return None;
        }
        offset = end;
    }
}

/// Check one staged unit before the engine can touch the terminal: optional
/// HISTORY manifests, then PAGE (including Kitty placements), IMAGE or FINISH.
/// Mirrors `FeedIncrementalDecoder`.
pub(super) fn history_unit(bytes: &[u8]) -> Option<HistoryUnit> {
    let mut offset = 0;
    let mut manifests = 0;
    loop {
        let (tag, payload, end) = snapshot_record(bytes, offset)?;
        match tag {
            SNAPSHOT_RECORD_HISTORY if manifests < 2 => {
                manifests += 1;
                offset = end;
            }
            SNAPSHOT_RECORD_PAGE => return page_unit(bytes, payload, end),
            SNAPSHOT_RECORD_IMAGE if end == bytes.len() => return Some(HistoryUnit::Image),
            SNAPSHOT_RECORD_FINISH if end == bytes.len() => return Some(HistoryUnit::Finish),
            tag if unknown_snapshot_record(tag) => offset = end,
            _ => return None,
        }
    }
}
