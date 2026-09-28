//! Negotiated frame compression (`docs/spec/proto.md` §6.4).
//!
//! `FRAME_COMPRESSED` wraps one deflated inner frame body (type byte plus
//! payload); the decoder inflates and dispatches it, so inner records stay
//! byte-identical. One wrapper frame covers the whole catalog instead of a
//! per-payload flag. DEFLATE via pure-Rust `miniz_oxide` (already in the
//! tree) gives ~14x on dense native bootstraps without a C dependency.

use flate2::{
    Compress, Compression as FlateLevel, Decompress, FlushCompress, FlushDecompress, Status,
};

use super::error::DecodeError;

/// Deflate level for outbound frames: level 6 compresses ~2x better at ~3x
/// the CPU on every attach's critical path, which only pays on slow links.
const LEVEL: u32 = 1;

/// Smallest frame body worth wrapping; interactive output stays below it and
/// never pays the compressor.
pub const MIN_COMPRESS_BYTES: usize = 4 * 1024;

/// Deflate `body` (one frame's type byte plus payload), or `None` when it is
/// below [`MIN_COMPRESS_BYTES`] or would not shrink; the caller then writes
/// the frame plain.
#[must_use]
pub fn deflate(body: &[u8]) -> Option<Vec<u8>> {
    if body.len() < MIN_COMPRESS_BYTES {
        return None;
    }
    // Capacity = input length: output that does not fit has not paid, and
    // `compress_vec` reports that as `BufError`, not `Err`, so the status is
    // checked or a truncated stream would slip through.
    let mut out = Vec::with_capacity(body.len());
    let mut compress = Compress::new(FlateLevel::new(LEVEL), false);
    let status = compress
        .compress_vec(body, &mut out, FlushCompress::Finish)
        .ok()?;
    (status == Status::StreamEnd && out.len() < body.len()).then_some(out)
}

/// Inflate a `FRAME_COMPRESSED` payload to exactly `uncompressed_len` bytes.
///
/// The buffer is allocated to that size (so a bomb cannot allocate more) and
/// must be filled exactly. The caller bounds `uncompressed_len` first.
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
    // Both checks are needed: without `StreamEnd` a short declared length
    // accepts a prefix; without the length check a rounded-up capacity lets
    // a longer stream through.
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
    fn round_trips_and_declines_small_bodies() {
        let body: Vec<u8> = (0..64_u32)
            .flat_map(|row| format!("{row:04} the quick brown fox jumps  \n").into_bytes())
            .cycle()
            .take(64 * 1024)
            .collect();
        let deflated = deflate(&body).expect("a repetitive body compresses");
        assert!(deflated.len() * 4 < body.len());
        assert_eq!(inflate(&deflated, body.len()).expect("inflates"), body);
        assert_eq!(deflate(&[0_u8; MIN_COMPRESS_BYTES - 1]), None);
    }

    /// `deflate` declines or returns something strictly smaller that inflates
    /// back, even for incompressible input.
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

    /// A declared length off by one either way, or a non-DEFLATE payload, is
    /// refused rather than truncated or padded.
    #[test]
    fn rejects_mismatched_lengths_and_garbage() {
        let body = vec![7_u8; MIN_COMPRESS_BYTES * 2];
        let deflated = deflate(&body).expect("compresses");
        for (payload, len) in [
            (deflated.as_slice(), body.len() - 1),
            (deflated.as_slice(), body.len() + 1),
            (b"not a deflate stream at all".as_slice(), 4096),
        ] {
            assert_eq!(
                inflate(payload, len),
                Err(DecodeError::CompressedFrameInvalid)
            );
        }
    }
}
