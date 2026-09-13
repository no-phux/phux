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
    cursor: usize,
    poisoned: bool,
    compacted_bytes: usize,
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
            self.compact_before_push(chunk.len());
            self.buf.extend_from_slice(chunk);
        }
    }

    /// Pull the next complete frame off the buffer, or `None` if more bytes
    /// are needed. Call in a loop after each [`push`](Self::push): one chunk
    /// may complete several frames.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        let available = self.buf.len().saturating_sub(self.cursor);
        if self.poisoned || available < LENGTH_PREFIX {
            return None;
        }
        let mut header = [0u8; LENGTH_PREFIX];
        header.copy_from_slice(&self.buf[self.cursor..self.cursor + LENGTH_PREFIX]);
        let body_len = u32::from_be_bytes(header);
        if !(1..=MAX_FRAME_LEN).contains(&body_len) {
            // A zero or oversized length means the stream is desynchronized
            // (or hostile); no later byte can be trusted to be a boundary.
            self.poisoned = true;
            self.buf.clear();
            self.cursor = 0;
            return None;
        }
        let total = LENGTH_PREFIX + body_len as usize;
        if available < total {
            return None;
        }
        let end = self.cursor + total;
        let frame = self.buf[self.cursor..end].to_vec();
        self.cursor = end;
        if self.cursor == self.buf.len() {
            self.buf.clear();
            self.cursor = 0;
        }
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
        self.buf.len().saturating_sub(self.cursor)
    }

    /// Bytes copied solely to compact an incomplete tail. Complete frames are
    /// excluded; tests use this to guard against quadratic tail copying.
    #[must_use]
    pub const fn compacted_bytes(&self) -> usize {
        self.compacted_bytes
    }

    fn compact_before_push(&mut self, incoming: usize) {
        if self.cursor == 0 {
            return;
        }
        let pending = self.pending_bytes();
        if self.buf.capacity().saturating_sub(self.buf.len()) >= incoming
            && self.cursor < self.buf.len() / 2
        {
            return;
        }
        self.buf.copy_within(self.cursor.., 0);
        self.buf.truncate(pending);
        self.cursor = 0;
        self.compacted_bytes = self.compacted_bytes.saturating_add(pending);
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
        assert_eq!(
            fb.compacted_bytes(),
            0,
            "draining must not copy frame tails"
        );
    }

    #[wasm_bindgen_test]
    fn many_small_frames_avoid_quadratic_tail_copying() {
        const COUNT: usize = 2_048;
        let mut fb = FrameBuffer::new();
        let stream = FRAME.repeat(COUNT);
        fb.push(&stream);
        for _ in 0..COUNT {
            assert_eq!(fb.next_frame().as_deref(), Some(&FRAME[..]));
        }
        assert_eq!(fb.pending_bytes(), 0);
        assert_eq!(fb.compacted_bytes(), 0);

        let old_split_off_tail_bytes = FRAME.len() * COUNT * (COUNT - 1) / 2;
        assert_eq!(old_split_off_tail_bytes, 14_672_896);
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
