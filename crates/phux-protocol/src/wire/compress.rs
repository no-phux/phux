//! Negotiated frame compression (`docs/spec/proto.md` §6.4).
//!
//! A `FRAME_COMPRESSED` frame carries one complete inner frame body — its type
//! byte and payload, i.e. everything after the length prefix — deflated. The
//! encoder produces it, the decoder inflates it and dispatches the result, and
//! nothing between those two points changes: an inner `BOOTSTRAP_CHUNK`
//! reaches its consumer byte-identical, which is what keeps this compatible
//! with §6.2's rule that native records stay byte-identical across server,
//! transport, recorder, and relay.
//!
//! **Why the wrapper rather than a `compressed` flag on each payload.** The
//! payload-flag shape has to be added to every frame worth compressing, and
//! each addition is a new pair of optional fields plus a new decoder branch
//! whose absence is indistinguishable from "not compressed". The wrapper is
//! one frame type that composes with the whole catalog, so `BOOTSTRAP_CHUNK`,
//! `HISTORY_PAGE`, and a bulk `RESOURCE_OUTPUT` burst are all covered by one
//! negotiation and one pair of functions.
//!
//! **Why DEFLATE and not zstd.** `flate2`'s `miniz_oxide` backend is already
//! in the lock file (transitively, via `png`), is pure safe Rust with no C
//! toolchain, and measured 14x on the hardest corpus phux actually ships — a
//! dense scrolled 200x50 native bootstrap prefix, 522 KiB down to 37 KiB at
//! level 1. zstd would do better on ratio, but it would add a real new
//! dependency with a C backend for a payload that is already 14x smaller, and
//! the remaining bytes are no longer what makes a remote first paint slow.

use flate2::{
    Compress, Compression as FlateLevel, Decompress, FlushCompress, FlushDecompress, Status,
};

use super::error::DecodeError;

/// Deflate level used for outbound frames.
///
/// Level 1, not the default 6. On the same 522 KiB corpus level 6 reaches 27x
/// against level 1's 14x, but it spends roughly three times the CPU to do it,
/// and that CPU is on the critical path of *every* attach while the extra
/// compression only pays on a slow link. 14x already turns a 522 KiB first
/// paint into 37 KiB, at which point the round trips dominate again.
const LEVEL: u32 = 1;

/// Smallest frame body worth wrapping.
///
/// Below this the deflate header plus the wrapper's own TLV fields cost more
/// than the transform saves, and a keystroke echo must never pay a compressor
/// at all. Sized above a full-width styled `RESOURCE_OUTPUT` line so ordinary
/// interactive output stays on the uncompressed path.
pub const MIN_COMPRESS_BYTES: usize = 4 * 1024;

/// Deflate `body` (one frame's type byte plus payload).
///
/// Returns `None` when the transform did not pay — the body was below
/// [`MIN_COMPRESS_BYTES`], or it compressed to no less than it started at
/// (already-compressed image data reaches phux this way). The caller then
/// writes the frame uncompressed, which is always legal: compression is
/// per-frame, not per-connection.
#[must_use]
pub fn deflate(body: &[u8]) -> Option<Vec<u8>> {
    if body.len() < MIN_COMPRESS_BYTES {
        return None;
    }
    // Capacity is the input length, and that is also the decision: the output
    // is only useful if it is smaller than the input, so a compressor that
    // wants more room than the body it is compressing has already lost. It
    // reports that as `BufError` rather than an `Err`, which is why the status
    // is checked and not just the result — `compress_vec` fills the spare
    // capacity it was given and never grows the vec, so ignoring `BufError`
    // would hand back a silently truncated stream.
    let mut out = Vec::with_capacity(body.len());
    let mut compress = Compress::new(FlateLevel::new(LEVEL), false);
    // `Finish` on the whole input in one call: the body is already fully in
    // memory, so there is no streaming to do and no partial state to keep.
    let status = compress
        .compress_vec(body, &mut out, FlushCompress::Finish)
        .ok()?;
    (status == Status::StreamEnd && out.len() < body.len()).then_some(out)
}

/// Inflate a `FRAME_COMPRESSED` payload back to exactly `uncompressed_len`
/// bytes.
///
/// `uncompressed_len` is authoritative and checked twice: the buffer is
/// allocated to exactly that size, so a decompression bomb cannot make the
/// receiver allocate more than the sender declared, and the result must fill
/// it exactly, so a truncated or over-long stream is rejected rather than
/// dispatched as a short frame. The caller is responsible for bounding
/// `uncompressed_len` itself against `MAX_FRAME_LEN` before calling.
///
/// # Errors
///
/// [`DecodeError::CompressedFrameInvalid`] when the payload is not a valid
/// DEFLATE stream, or does not inflate to exactly `uncompressed_len` bytes.
pub fn inflate(payload: &[u8], uncompressed_len: usize) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(uncompressed_len);
    let mut decompress = Decompress::new(false);
    let status = decompress
        .decompress_vec(payload, &mut out, FlushDecompress::Finish)
        .map_err(|_| DecodeError::CompressedFrameInvalid)?;
    // Both conditions are load-bearing, and neither implies the other.
    //
    // `StreamEnd` proves the DEFLATE stream ran to completion; without it, a
    // sender could declare a length short of the real output and get the
    // *prefix* accepted, because `decompress_vec` stops at the buffer's
    // capacity and reports `BufError` rather than failing. `out.len()` proves
    // the completed stream is exactly the length declared; without it, an
    // allocator that rounded `with_capacity` up would let a longer stream
    // through. Together they pin the output to exactly what the sender said,
    // which is the only property the caller's `MAX_FRAME_LEN` check can rest
    // on.
    if status != Status::StreamEnd || out.len() != uncompressed_len {
        return Err(DecodeError::CompressedFrameInvalid);
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_compressible_body() {
        let body: Vec<u8> = (0..64_u32)
            .flat_map(|row| format!("{row:04} the quick brown fox jumps  \n").into_bytes())
            .cycle()
            .take(64 * 1024)
            .collect();
        let deflated = deflate(&body).expect("a repetitive body compresses");
        assert!(
            deflated.len() * 4 < body.len(),
            "expected a real reduction, got {} from {}",
            deflated.len(),
            body.len()
        );
        assert_eq!(inflate(&deflated, body.len()).expect("inflates"), body);
    }

    #[test]
    fn declines_a_body_below_the_threshold() {
        assert_eq!(
            deflate(&[0_u8; MIN_COMPRESS_BYTES - 1]),
            None,
            "an interactive-sized frame must never reach the compressor"
        );
    }

    /// The contract on the output side, whatever the input: `deflate` either
    /// declines, or returns something **strictly smaller** that inflates back
    /// to the original. The caller relies on exactly that — it writes the
    /// plain frame on `None` — so a backend whose framing overhead exceeded
    /// the input would degrade throughput, not correctness, and this pins it.
    #[test]
    fn never_returns_a_body_that_grew() {
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let noise: Vec<u8> = (0..64 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                u8::try_from((state >> 24) & 0xff).unwrap_or_default()
            })
            .collect();
        if let Some(deflated) = deflate(&noise) {
            assert!(
                deflated.len() < noise.len(),
                "deflate returned {} bytes for a {}-byte body",
                deflated.len(),
                noise.len()
            );
            assert_eq!(inflate(&deflated, noise.len()).expect("inflates"), noise);
        }
    }

    #[test]
    fn rejects_a_length_that_does_not_match() {
        let body = vec![7_u8; MIN_COMPRESS_BYTES * 2];
        let deflated = deflate(&body).expect("compresses");
        assert_eq!(
            inflate(&deflated, body.len() - 1),
            Err(DecodeError::CompressedFrameInvalid),
            "a declared length shorter than the stream must be refused, not \
             silently truncated to the prefix that fits"
        );
        assert_eq!(
            inflate(&deflated, body.len() + 1),
            Err(DecodeError::CompressedFrameInvalid),
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(
            inflate(b"not a deflate stream at all", 4096),
            Err(DecodeError::CompressedFrameInvalid),
        );
    }
}
