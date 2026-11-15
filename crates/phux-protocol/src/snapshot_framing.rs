//! Pure-Rust framing and CRC32C validation for bounded GHOSTSNP units.
//!
//! Validate a complete history unit before allowing the engine to mutate its
//! READY terminal. The engine still validates the snapshot's semantic content.
//! [`HistoryFraming`] adapts bare PAGE units to the stock decoder's lookahead
//! without changing the wire bytes or requiring engine-specific entry points.

#[cfg(test)]
mod tests;

// `{ tag: u16, payload_len: u32, crc32c: u32 }`.
const SNAPSHOT_RECORD_HEADER_BYTES: usize = 10;
const SNAPSHOT_RECORD_PAGE: u16 = 3;
const SNAPSHOT_RECORD_HISTORY: u16 = 4;
const SNAPSHOT_RECORD_FINISH: u16 = 6;
const SNAPSHOT_RECORD_IMAGE: u16 = 9;
const SNAPSHOT_RECORD_PAGE_PLACEMENTS: u16 = 10;

const CRC32C_TABLE: [u32; 256] = crc32c_table();

const fn crc32c_table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut index = 0_u32;
    while index < 256 {
        let mut crc = index;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0x82f6_3b78
            };
            bit += 1;
        }
        table[index as usize] = crc;
        index += 1;
    }
    table
}

/// Castagnoli CRC over the six-byte tag/length prefix followed by the payload;
/// the stored CRC itself is deliberately excluded.
fn crc32c(prefix: &[u8], payload: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in prefix.iter().chain(payload) {
        crc = CRC32C_TABLE[usize::from(crc.to_le_bytes()[0] ^ byte)] ^ (crc >> 8);
    }
    !crc
}

/// One CRC-valid record's `(tag, payload start, end)`, if it fits `bytes`.
fn snapshot_record(bytes: &[u8], offset: usize) -> Option<(u16, usize, usize)> {
    let payload = offset.checked_add(SNAPSHOT_RECORD_HEADER_BYTES)?;
    let header = bytes.get(offset..payload)?;
    let tag = u16::from_le_bytes([header[0], header[1]]);
    let len = u32::from_le_bytes([header[2], header[3], header[4], header[5]]) as usize;
    let end = payload.checked_add(len).filter(|end| *end <= bytes.len())?;
    let stored_crc = u32::from_le_bytes(header[6..10].try_into().ok()?);
    if crc32c(&header[..6], &bytes[payload..end]) != stored_crc {
        return None;
    }
    Some((tag, payload, end))
}

/// The end of the CRC-valid READY prefix, or `None` for corrupt/truncated input.
///
/// Validates the GHOSTSNP v1 envelope and every record through READY. Bytes after
/// READY are not inspected: the returned offset splits the prefix from history.
#[must_use]
pub fn snapshot_ready_end(bytes: &[u8]) -> Option<usize> {
    const ENVELOPE_BYTES: usize = 10;
    const RECORD_READY: u16 = 5;
    if bytes.get(..ENVELOPE_BYTES)? != b"GHOSTSNP\x01\0" {
        return None;
    }
    let mut offset = ENVELOPE_BYTES;
    loop {
        let (tag, _, end) = snapshot_record(bytes, offset)?;
        if tag == RECORD_READY {
            return Some(end);
        }
        offset = end;
    }
}

/// One authenticated history progress unit, excluding its HISTORY manifests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryUnit {
    /// One history page and any associated Kitty placement record.
    Page {
        /// Authenticated row count in the PAGE header.
        rows: usize,
        /// Whether this unit already ends with a Kitty placements record.
        placements: bool,
    },
    /// Kitty image or animation-frame pixels, with no history rows.
    Image,
    /// The end of the snapshot.
    Finish,
}

impl HistoryUnit {
    /// Authenticated PAGE rows, or zero for IMAGE and FINISH.
    #[must_use]
    pub const fn rows(self) -> usize {
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

const fn unknown_snapshot_record(tag: u16) -> bool {
    tag == 0 || tag > SNAPSHOT_RECORD_PAGE_PLACEMENTS
}

/// Optional extension records may precede placements, but a second known
/// progress record must never be folded into a PAGE unit.
fn page_placements(bytes: &[u8], mut offset: usize) -> Option<bool> {
    while offset < bytes.len() {
        let (tag, _, end) = snapshot_record(bytes, offset)?;
        if tag == SNAPSHOT_RECORD_PAGE_PLACEMENTS && end == bytes.len() {
            return Some(true);
        }
        if !unknown_snapshot_record(tag) {
            return None;
        }
        offset = end;
    }
    Some(false)
}

/// Classify exactly one CRC-valid history unit before the engine can mutate.
///
/// Accepts at most two HISTORY manifests, then PAGE (with optional Kitty
/// placements), IMAGE or FINISH. Unknown records are skipped, but their CRCs
/// are checked too. Truncation, a second progress unit or corrupt bytes fail.
#[must_use]
pub fn history_unit(bytes: &[u8]) -> Option<HistoryUnit> {
    history_framing(bytes).map(|(unit, _)| unit)
}

fn history_framing(bytes: &[u8]) -> Option<(HistoryUnit, Option<u16>)> {
    let mut offset = 0;
    let mut manifests = 0;
    let mut screen_key = None;
    loop {
        let (tag, payload, end) = snapshot_record(bytes, offset)?;
        match tag {
            SNAPSHOT_RECORD_HISTORY if manifests < 2 => {
                screen_key = Some(history_screen_key(&bytes[payload..end])?);
                manifests += 1;
                offset = end;
            }
            SNAPSHOT_RECORD_PAGE => return Some((page_unit(bytes, payload, end)?, screen_key)),
            SNAPSHOT_RECORD_IMAGE if end == bytes.len() => {
                return Some((HistoryUnit::Image, screen_key));
            }
            SNAPSHOT_RECORD_FINISH if end == bytes.len() => {
                return Some((HistoryUnit::Finish, screen_key));
            }
            tag if unknown_snapshot_record(tag) => offset = end,
            _ => return None,
        }
    }
}

const fn history_screen_key(payload: &[u8]) -> Option<u16> {
    if payload.len() != 6 {
        return None;
    }
    Some(u16::from_le_bytes([payload[0], payload[1]]))
}

/// Tracks the HISTORY sequence's screen key for stock decoder PAGE lookahead.
///
/// A bare PAGE makes the stock decoder look for an optional placements record;
/// EOF there is an error, and unknown records are skipped rather than ending
/// lookahead. [`Self::prepare`] supplies a valid empty placements record instead.
/// Append it only to the local staged reader, never to the transport or capture.
/// Requires a decoder built with Kitty graphics support so tag 10 is recognized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoryFraming {
    screen_key: Option<u16>,
}

impl HistoryFraming {
    /// Validate a complete unit and prepare its optional local no-op suffix.
    ///
    /// The last HISTORY manifest selects the sequence key, independent of the
    /// live terminal's active screen. A bare PAGE without any prior manifest
    /// fails. Existing placements, IMAGE and FINISH need no suffix. Invalid
    /// input never changes the tracked key.
    ///
    /// Check the returned unit's [`HistoryUnit::rows`] against the transport
    /// before calling the engine. If rejected units must also leave this helper
    /// unchanged, prepare on a copy and commit the copy only after that check.
    pub fn prepare(&mut self, bytes: &[u8]) -> Option<(HistoryUnit, Option<[u8; 16]>)> {
        let (unit, manifest_key) = history_framing(bytes)?;
        let screen_key = manifest_key.or(self.screen_key);
        let suffix = match unit {
            HistoryUnit::Page {
                placements: false, ..
            } => Some(empty_placements(screen_key?)),
            _ => None,
        };
        self.screen_key = screen_key;
        Some((unit, suffix))
    }
}

fn empty_placements(screen_key: u16) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[..2].copy_from_slice(&SNAPSHOT_RECORD_PAGE_PLACEMENTS.to_le_bytes());
    bytes[2..6].copy_from_slice(&6_u32.to_le_bytes());
    bytes[10..12].copy_from_slice(&screen_key.to_le_bytes());
    let crc = crc32c(&bytes[..6], &bytes[10..]);
    bytes[6..10].copy_from_slice(&crc.to_le_bytes());
    bytes
}
