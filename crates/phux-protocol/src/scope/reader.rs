//! Byte helpers shared by the scope codecs.

use super::ScopeError;
use super::selector::{Host, Selector};

// -----------------------------------------------------------------------------
// Byte helpers.
// -----------------------------------------------------------------------------

pub(super) fn push_count(out: &mut Vec<u8>, count: usize) {
    // Both sets are bounded at 64 entries, so the count always fits.
    let count = u16::try_from(count).unwrap_or(u16::MAX);
    out.extend_from_slice(&count.to_be_bytes());
}

pub(super) fn push_selector(out: &mut Vec<u8>, selector: &Selector) {
    let bytes = selector.to_bytes();
    // The longest selector is a satellite Terminal: 2 + 2 + 255 + 4 bytes.
    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&bytes);
}

/// A bounds-checked big-endian reader over one image.
pub(super) struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(super) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    const fn take(&mut self, len: usize) -> Result<&'a [u8], ScopeError> {
        if self.bytes.len() < len {
            return Err(ScopeError::Truncated);
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(head)
    }

    pub(super) fn u8(&mut self) -> Result<u8, ScopeError> {
        Ok(self.take(1)?[0])
    }

    /// A `U16` entry count, refused with `too_many` above `max`.
    pub(super) fn count(&mut self, max: usize, too_many: ScopeError) -> Result<usize, ScopeError> {
        let count = usize::from(self.u16()?);
        if count > max {
            return Err(too_many);
        }
        Ok(count)
    }

    fn u16(&mut self) -> Result<u16, ScopeError> {
        let raw = self.take(2)?;
        Ok(u16::from_be_bytes([raw[0], raw[1]]))
    }

    pub(super) fn u32(&mut self) -> Result<u32, ScopeError> {
        let raw = self.take(4)?;
        Ok(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }

    /// `U16(len) || selector_bytes`, returned raw for order checks.
    pub(super) fn selector_bytes(&mut self) -> Result<Vec<u8>, ScopeError> {
        let len = usize::from(self.u16()?);
        Ok(self.take(len)?.to_vec())
    }

    /// A 16-byte key, refused when `new` rejects it (all zero).
    pub(super) fn key<K>(
        &mut self,
        new: impl FnOnce([u8; 16]) -> Option<K>,
    ) -> Result<K, ScopeError> {
        let mut bytes = [0; 16];
        bytes.copy_from_slice(self.take(16)?);
        new(bytes).ok_or(ScopeError::InvalidKey)
    }

    /// `V16(host)`, validated.
    pub(super) fn host(&mut self) -> Result<Host, ScopeError> {
        let len = usize::from(self.u16()?);
        let raw = self.take(len)?;
        let text = std::str::from_utf8(raw).map_err(|_| ScopeError::InvalidHost)?;
        Host::new(text)
    }

    pub(super) const fn finish(&self) -> Result<(), ScopeError> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(ScopeError::TrailingBytes)
        }
    }
}
