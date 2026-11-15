//! The freestanding engine has no Kitty graphics support. Authenticate complete
//! wire units first, then keep unsupported pixels/placements out of its reader.

use phux_protocol::snapshot_framing::HistoryUnit;

use crate::NativeCodecError;

const MANIFEST_BYTES: usize = 16;

/// At most two screens' HISTORY manifests may wait behind skipped IMAGE units.
#[derive(Clone, Default)]
pub(super) struct BrowserHistory {
    manifests: [u8; 2 * MANIFEST_BYTES],
    len: usize,
}

impl BrowserHistory {
    /// Called only after full CRC/framing and transport-row validation. IMAGE
    /// returns no engine input; PAGE stops at its own end, before placements.
    pub(super) fn stage(
        &mut self,
        mut unit: Vec<u8>,
        kind: HistoryUnit,
    ) -> Result<Option<Vec<u8>>, NativeCodecError> {
        let mut pending = self.clone();
        let end = pending.capture_manifests(&unit)?;
        if kind == HistoryUnit::Image {
            *self = pending;
            return Ok(None);
        }
        unit.truncate(end);
        self.prepend_manifests(&mut unit)?;
        self.len = 0;
        Ok(Some(unit))
    }

    fn capture_manifests(&mut self, bytes: &[u8]) -> Result<usize, NativeCodecError> {
        let mut offset = 0;
        loop {
            let (tag, end) = record_end(bytes, offset).ok_or(NativeCodecError::InvalidUnit)?;
            if tag == 4 {
                self.push_manifest(&bytes[offset..end])?;
            }
            if matches!(tag, 3 | 6 | 9) {
                return Ok(end);
            }
            offset = end;
        }
    }

    fn push_manifest(&mut self, record: &[u8]) -> Result<(), NativeCodecError> {
        if record.len() != MANIFEST_BYTES || self.len == self.manifests.len() {
            return Err(NativeCodecError::InvalidUnit);
        }
        self.manifests[self.len..self.len + MANIFEST_BYTES].copy_from_slice(record);
        self.len += MANIFEST_BYTES;
        Ok(())
    }

    fn prepend_manifests(&self, unit: &mut Vec<u8>) -> Result<(), NativeCodecError> {
        unit.try_reserve_exact(self.len)
            .map_err(|_| NativeCodecError::OutOfMemory)?;
        let end = unit.len();
        unit.resize(end + self.len, 0);
        unit.copy_within(..end, self.len);
        unit[..self.len].copy_from_slice(&self.manifests[..self.len]);
        Ok(())
    }
}

/// Defensive bounds checks remain even though the shared framing preflight
/// already validated these lengths and every record's CRC.
fn record_end(bytes: &[u8], offset: usize) -> Option<(u16, usize)> {
    let payload = offset.checked_add(10)?;
    let header = bytes.get(offset..payload)?;
    let tag = u16::from_le_bytes(header[..2].try_into().ok()?);
    let len = u32::from_le_bytes(header[2..6].try_into().ok()?) as usize;
    let end = payload.checked_add(len).filter(|end| *end <= bytes.len())?;
    Some((tag, end))
}

#[cfg(test)]
mod tests;
