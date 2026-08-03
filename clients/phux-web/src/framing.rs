//! Byte-stream reframing for stream-shaped transports (WebTransport).
//!
//! The WebSocket path delivers one complete encoded frame per binary message,
//! so no reassembly is needed there. A WebTransport bidirectional stream is a
//! plain byte stream — chunks arrive at arbitrary boundaries — so the reader
//! accumulates bytes here and pulls out complete length-prefixed frames
//! (`docs/spec/proto.md` §5: 4-byte big-endian length, then `length` bytes),
//! exactly the reassembly the server's UDS/QUIC readers perform on their side.

use phux_protocol::wire::frame::MAX_FRAME_LEN;

/// Length-prefix size in bytes (`docs/spec/proto.md` §5).
const LENGTH_PREFIX: usize = 4;

/// Accumulates stream chunks and yields complete encoded frames
/// (length prefix included, so [`FrameKind::decode`] applies directly).
///
/// [`FrameKind::decode`]: phux_protocol::wire::frame::FrameKind::decode
#[derive(Default)]
pub struct FrameBuffer {
    buf: Vec<u8>,
    poisoned: bool,
}

impl FrameBuffer {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a received chunk.
    pub fn push(&mut self, chunk: &[u8]) {
        if !self.poisoned {
            self.buf.extend_from_slice(chunk);
        }
    }

    /// Pull the next complete frame off the buffer, or `None` if more bytes
    /// are needed. Call in a loop after each [`push`](Self::push): one chunk
    /// may complete several frames.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        if self.poisoned || self.buf.len() < LENGTH_PREFIX {
            return None;
        }
        let mut header = [0u8; LENGTH_PREFIX];
        header.copy_from_slice(&self.buf[..LENGTH_PREFIX]);
        let body_len = u32::from_be_bytes(header);
        if !(1..=MAX_FRAME_LEN).contains(&body_len) {
            // A zero or oversized length means the stream is desynchronized
            // (or hostile); no later byte can be trusted to be a boundary.
            self.poisoned = true;
            self.buf.clear();
            return None;
        }
        let total = LENGTH_PREFIX + body_len as usize;
        if self.buf.len() < total {
            return None;
        }
        let rest = self.buf.split_off(total);
        let frame = std::mem::replace(&mut self.buf, rest);
        Some(frame)
    }

    /// Whether the stream desynchronized (an out-of-bounds length was seen).
    /// A poisoned buffer yields no further frames; the transport should be
    /// closed.
    #[must_use]
    pub const fn poisoned(&self) -> bool {
        self.poisoned
    }

    /// Bytes retained for an incomplete frame.
    #[must_use]
    pub fn pending_bytes(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// One encoded frame: length prefix (3) + three body bytes.
    const FRAME: [u8; 7] = [0, 0, 0, 3, 0xde, 0xad, 0xbe];

    #[wasm_bindgen_test]
    fn reassembles_across_arbitrary_chunk_boundaries() {
        let mut fb = FrameBuffer::new();
        // Two frames split awkwardly across three chunks.
        let mut stream = Vec::new();
        stream.extend_from_slice(&FRAME);
        stream.extend_from_slice(&FRAME);
        fb.push(&stream[..2]);
        assert!(fb.next_frame().is_none(), "header incomplete");
        fb.push(&stream[2..9]);
        assert_eq!(fb.next_frame().as_deref(), Some(&FRAME[..]));
        assert!(fb.next_frame().is_none(), "second frame incomplete");
        fb.push(&stream[9..]);
        assert_eq!(fb.next_frame().as_deref(), Some(&FRAME[..]));
        assert!(fb.next_frame().is_none());
        assert!(!fb.poisoned());
    }

    #[wasm_bindgen_test]
    fn one_chunk_may_hold_many_frames() {
        let mut fb = FrameBuffer::new();
        let mut stream = Vec::new();
        for _ in 0..3 {
            stream.extend_from_slice(&FRAME);
        }
        fb.push(&stream);
        assert_eq!(fb.next_frame().as_deref(), Some(&FRAME[..]));
        assert_eq!(fb.next_frame().as_deref(), Some(&FRAME[..]));
        assert_eq!(fb.next_frame().as_deref(), Some(&FRAME[..]));
        assert!(fb.next_frame().is_none());
    }

    #[wasm_bindgen_test]
    fn zero_length_poisons() {
        let mut fb = FrameBuffer::new();
        fb.push(&[0, 0, 0, 0, 1, 2, 3]);
        assert!(fb.next_frame().is_none());
        assert!(fb.poisoned());
        // Poisoned buffers stay dead.
        fb.push(&FRAME);
        assert!(fb.next_frame().is_none());
    }

    #[wasm_bindgen_test]
    fn oversized_length_poisons() {
        let mut fb = FrameBuffer::new();
        fb.push(&u32::MAX.to_be_bytes());
        assert!(fb.next_frame().is_none());
        assert!(fb.poisoned());
    }
}
