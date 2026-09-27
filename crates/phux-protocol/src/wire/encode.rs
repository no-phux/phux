//! Wire-frame encoder (`docs/spec/appendix-encoding.md`).
//!
//! Message bodies are field-tagged TLV; inside a field's value, primitives
//! are positional and big-endian, with `u32`-length-prefixed strings and
//! bytes.

use bytes::BytesMut;

/// TLV wire types (`docs/spec/appendix-encoding.md` §1).
pub mod wire_type {
    /// Length-delimited `varint length || bytes`: the only type emitted at the
    /// body level, so an unknown field always skips by length.
    pub const BYTES: u8 = 4;
}

/// Primitive encoder over a borrowed `BytesMut`.
#[derive(Debug)]
pub struct Encoder<'a> {
    buf: &'a mut BytesMut,
    field_scratch: Option<BytesMut>,
}

impl<'a> Encoder<'a> {
    /// Wrap `buf` for primitive writes.
    #[must_use]
    pub const fn new(buf: &'a mut BytesMut) -> Self {
        Self {
            buf,
            field_scratch: None,
        }
    }

    /// Borrow the underlying buffer.
    #[must_use]
    pub const fn buffer(&self) -> &BytesMut {
        self.buf
    }

    /// Total bytes currently in the underlying buffer.
    #[must_use]
    pub fn position(&self) -> usize {
        self.buf.len()
    }

    /// Write one unsigned byte.
    pub fn write_u8(&mut self, value: u8) {
        self.buf.extend_from_slice(&[value]);
    }

    /// Write a big-endian `u16`.
    pub fn write_u16_be(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// Write a big-endian `u32`.
    pub fn write_u32_be(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// Write a big-endian `u64`.
    pub fn write_u64_be(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// Write a big-endian two's-complement `i64`.
    pub fn write_i64_be(&mut self, value: i64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// Write a big-endian IEEE-754 `f32`, bit for bit.
    pub fn write_f32_be(&mut self, value: f32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// Write a big-endian IEEE-754 `f64`, bit for bit.
    pub fn write_f64_be(&mut self, value: f64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// Write a `u32`-length-prefixed UTF-8 string.
    pub fn write_str(&mut self, value: &str) {
        self.write_bytes(value.as_bytes());
    }

    /// Write a presence byte (`0` = `None`, `1` = `Some`) and, when present,
    /// the value via `body`.
    pub(crate) fn write_option<T>(&mut self, value: Option<T>, body: impl FnOnce(&mut Self, T)) {
        match value {
            None => self.write_u8(0),
            Some(value) => {
                self.write_u8(1);
                body(self, value);
            }
        }
    }

    /// Write a `u32`-length-prefixed byte slice (saturating in release; the
    /// decoder rejects the oversized frame).
    pub fn write_bytes(&mut self, value: &[u8]) {
        debug_assert!(
            u32::try_from(value.len()).is_ok(),
            "length-prefixed payload exceeds u32",
        );
        let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
        self.write_u32_be(len);
        self.buf.extend_from_slice(value);
    }

    /// Write an unsigned LEB128 varint.
    pub fn write_varint(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.buf.extend_from_slice(&[byte]);
                break;
            }
            self.buf.extend_from_slice(&[byte | 0x80]);
        }
    }

    /// Write one body-level TLV field: `field_id || BYTES || varint length ||
    /// value` (`docs/spec/appendix-encoding.md` §1). Absent values are simply
    /// not written.
    pub fn write_field(&mut self, field_id: u32, value: &[u8]) {
        self.write_varint(u64::from(field_id));
        self.write_u8(wire_type::BYTES);
        debug_assert!(
            u32::try_from(value.len()).is_ok(),
            "TLV field value exceeds u32",
        );
        self.write_varint(value.len() as u64);
        self.buf.extend_from_slice(value);
    }

    /// Write one TLV field whose value `build` writes positionally into an
    /// empty scratch [`Encoder`].
    pub fn write_field_with<F>(&mut self, field_id: u32, build: F)
    where
        F: FnOnce(&mut Encoder<'_>),
    {
        // Move scratch out so recursive field builders get their own buffer,
        // and a panicking builder cannot publish a partial field to `buf`.
        let mut scratch = self.field_scratch.take().unwrap_or_default();
        scratch.clear();
        {
            let mut sub = Encoder::new(&mut scratch);
            build(&mut sub);
        }
        self.write_field(field_id, &scratch);
        self.field_scratch = Some(scratch);
    }
}

#[cfg(test)]
mod tests {
    use super::Encoder;
    use bytes::BytesMut;

    #[test]
    fn reused_fields_preserve_nested_zero_based_builders_and_wire_bytes() {
        let mut bytes = BytesMut::new();
        let mut encoder = Encoder::new(&mut bytes);
        encoder.write_u8(99);
        for value in [7, 8] {
            encoder.write_field_with(1, |field| {
                assert_eq!(field.position(), 0);
                assert!(field.buffer().is_empty());
                field.write_field_with(2, |nested| {
                    assert_eq!(nested.position(), 0);
                    nested.write_u8(value);
                });
                assert_eq!(field.buffer().as_ref(), &[2, 4, 1, value]);
            });
        }
        assert_eq!(
            bytes.as_ref(),
            &[99, 1, 4, 4, 2, 4, 1, 7, 1, 4, 4, 2, 4, 1, 8]
        );
    }

    #[test]
    fn builder_panic_leaves_preexisting_output_intact() {
        let mut bytes = BytesMut::new();
        let mut encoder = Encoder::new(&mut bytes);
        encoder.write_field_with(1, |field| field.write_u8(7));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            encoder.write_field_with(2, |field| {
                field.write_u64_be(99);
                panic!("interrupted builder");
            });
        }));
        assert!(result.is_err());
        encoder.write_field_with(3, |field| field.write_u8(8));
        assert_eq!(bytes.as_ref(), &[1, 4, 1, 7, 3, 4, 1, 8]);
    }
}
