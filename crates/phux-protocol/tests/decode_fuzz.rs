//! Structure-aware decode fuzz: every public decoder over untrusted bytes.
//!
//! `decode_never_panics` in the wire contract feeds raw random bytes, which
//! almost never survive the length header and type byte, so the per-frame
//! body decoders stay unexercised. This loop builds frames whose outer
//! framing is valid for every one of the 256 type bytes and whose body is a
//! random walk over the encoding grammar (TLV fields, `u32`-length byte
//! strings, varints, fixed-width integers, nested to a bounded depth), then
//! mutates the ones that decode. A server decodes these bytes from any peer
//! that reaches its socket, so a panic here is a remote denial of service.
//!
//! Deterministic: a fixed seed, overridable with `PHUX_DECODE_FUZZ_SEED`,
//! and `PHUX_DECODE_FUZZ_ITERS` raises the per-type budget for a longer
//! soak. A failure prints the seed and the offending bytes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::panic::{AssertUnwindSafe, catch_unwind};

use phux_protocol::caps::BootstrapLimits;
use phux_protocol::scope::{EffectiveScopeSet, Selector, TerminalScopeSet};
use phux_protocol::wire::decode::Decoder;
use phux_protocol::wire::frame::{FrameKind, decode_session_keep_empty, decode_session_rename};
use phux_protocol::wire::listeners::RemoteListenersReport;
use phux_protocol::wire::{compress, ssh_origin, stream_bind};

mod common;
use common::put_varint;

/// xorshift64*: tiny, dependency-free, reproducible.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    const fn byte(&mut self) -> u8 {
        self.next().to_le_bytes()[0]
    }

    fn bytes(&mut self, max: u64) -> Vec<u8> {
        let len = self.below(max + 1);
        (0..len).map(|_| self.byte()).collect()
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A small integer, biased towards the boundaries decoders branch on.
fn interesting(rng: &mut Rng) -> u64 {
    match rng.below(8) {
        0 => 0,
        1 => 1,
        2 => rng.below(16),
        3 => rng.below(256),
        4 => u64::from(u32::MAX),
        5 => u64::MAX,
        6 => u64::from(u16::MAX),
        _ => rng.next(),
    }
}

/// Append one grammar element; `depth` bounds nesting.
fn element(rng: &mut Rng, out: &mut Vec<u8>, depth: u32) {
    let nested = depth < 4;
    match rng.below(if nested { 10 } else { 7 }) {
        0 => out.push(u8::try_from(interesting(rng) & 0xff).unwrap()),
        1 => out.extend_from_slice(
            &u16::try_from(interesting(rng) & 0xffff)
                .unwrap()
                .to_be_bytes(),
        ),
        2 => out.extend_from_slice(
            &u32::try_from(interesting(rng) & 0xffff_ffff)
                .unwrap()
                .to_be_bytes(),
        ),
        3 => out.extend_from_slice(&interesting(rng).to_be_bytes()),
        4 => put_varint(out, interesting(rng)),
        5 => out.extend_from_slice(&rng.bytes(24)),
        6 => {
            // A u32-length string that may lie about its length.
            let body = rng.bytes(32);
            let len = if rng.below(8) == 0 {
                u32::try_from(interesting(rng) & 0xffff_ffff).unwrap()
            } else {
                u32::try_from(body.len()).unwrap()
            };
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(&body);
        }
        7 => {
            // A u32-length blob whose content is itself grammar.
            let mut inner = Vec::new();
            sequence(rng, &mut inner, depth + 1);
            out.extend_from_slice(&u32::try_from(inner.len()).unwrap().to_be_bytes());
            out.extend_from_slice(&inner);
        }
        _ => tlv(rng, out, depth + 1),
    }
}

fn sequence(rng: &mut Rng, out: &mut Vec<u8>, depth: u32) {
    for _ in 0..rng.below(5) {
        element(rng, out, depth);
    }
}

/// One TLV field with a small id (the ones decoders actually match on).
fn tlv(rng: &mut Rng, out: &mut Vec<u8>, depth: u32) {
    let mut value = Vec::new();
    sequence(rng, &mut value, depth);
    put_varint(out, rng.below(24));
    out.push(if rng.below(16) == 0 { rng.byte() } else { 4 });
    let len = if rng.below(16) == 0 {
        interesting(rng)
    } else {
        value.len() as u64
    };
    put_varint(out, len);
    out.extend_from_slice(&value);
}

/// Frame types the default seed gets to decode cleanly (20 when written).
const MIN_PARSED_TYPES: u32 = 16;

fn frame(type_byte: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.extend_from_slice(&u32::try_from(body.len() + 1).unwrap().to_be_bytes());
    out.push(type_byte);
    out.extend_from_slice(body);
    out
}

fn mutate(rng: &mut Rng, input: &[u8]) -> Vec<u8> {
    let mut out = input.to_vec();
    for _ in 0..=rng.below(4) {
        if out.len() <= 5 {
            break;
        }
        // Keep the 4-byte header and type byte so the body decoder is reached.
        let at = 5 + usize::try_from(rng.below(out.len() as u64 - 5)).unwrap();
        match rng.below(4) {
            0 => out[at] = rng.byte(),
            1 => out[at] ^= 1 << rng.below(8),
            2 => {
                let mut extra = Vec::new();
                element(rng, &mut extra, 2);
                out.splice(at..at, extra);
            }
            _ => out.truncate(at),
        }
    }
    let body_len = u32::try_from(out.len() - 4).unwrap();
    out[..4].copy_from_slice(&body_len.to_be_bytes());
    out
}

fn decode_all(bytes: &[u8]) {
    let _ = FrameKind::decode(bytes);
    let small = BootstrapLimits::new(64, 64).unwrap_or_default();
    let _ = FrameKind::decode_with_limits(bytes, small);
    // Trailing frames: the stream readers loop on the tail.
    let mut dec = Decoder::new(bytes);
    while dec.read_frame().is_ok() {}
    let _ = ssh_origin::restamp_hello(bytes, None);
}

fn decode_fragments(bytes: &[u8]) {
    let _ = Selector::decode(bytes);
    let _ = TerminalScopeSet::decode(bytes);
    let _ = EffectiveScopeSet::decode(bytes);
    let _ = stream_bind::decode(bytes);
    let _ = decode_session_rename(bytes);
    let _ = decode_session_keep_empty(bytes);
    let _ = compress::inflate(bytes, bytes.len().saturating_mul(3).min(1 << 16));
    if let Ok(text) = std::str::from_utf8(bytes) {
        let _ = RemoteListenersReport::from_json(text);
    }
}

fn check(seed: u64, what: &str, bytes: &[u8], run: fn(&[u8])) {
    assert!(
        catch_unwind(AssertUnwindSafe(|| run(bytes))).is_ok(),
        "decoder panicked ({what}, seed {seed:#x}) on input {bytes:02x?}"
    );
}

#[test]
fn every_frame_body_decoder_survives_structured_garbage() {
    let seed = env_u64("PHUX_DECODE_FUZZ_SEED", 0x9E37_79B9_7F4A_7C15);
    let iters = env_u64("PHUX_DECODE_FUZZ_ITERS", 300);
    let mut rng = Rng(seed | 1);
    let mut parsed_types = 0_u32;
    for type_byte in 0..=u8::MAX {
        let mut parsed = false;
        for _ in 0..iters {
            let mut body = Vec::new();
            for _ in 0..rng.below(6) {
                if rng.below(4) == 0 {
                    element(&mut rng, &mut body, 1);
                } else {
                    tlv(&mut rng, &mut body, 0);
                }
            }
            let bytes = frame(type_byte, &body);
            check(seed, "frame", &bytes, decode_all);
            let decoded = FrameKind::decode(&bytes).is_ok();
            parsed |= decoded;
            // Mutating a frame that parsed probes the paths just past a
            // successful field, where a stale assumption is most likely.
            for _ in 0..if decoded { 8 } else { 1 } {
                check(seed, "mutated frame", &mutate(&mut rng, &bytes), decode_all);
            }
        }
        parsed_types += u32::from(parsed);
    }
    // Keep the generator honest: if it stops producing frames the body
    // decoders accept, the mutations above stop reaching them.
    assert!(
        parsed_types >= MIN_PARSED_TYPES,
        "only {parsed_types} frame types ever decoded; the generator has degenerated"
    );
}

#[test]
fn every_fragment_decoder_survives_structured_garbage() {
    let seed = env_u64("PHUX_DECODE_FUZZ_SEED", 0xD1B5_4A32_D192_ED03);
    let iters = env_u64("PHUX_DECODE_FUZZ_ITERS", 300) * 64;
    let mut rng = Rng(seed | 1);
    for _ in 0..iters {
        let mut bytes = Vec::new();
        sequence(&mut rng, &mut bytes, 0);
        check(seed, "fragment", &bytes, decode_fragments);
    }
}
