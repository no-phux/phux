//! Keystroke-echo hop trace.
//!
//! `PHUX_ECHO_HOPS` is a path. When it is set, each hop appends one line
//! `pid <TAB> hop <TAB> unix_nanos` so a bench can diff the client and the
//! server on one clock. Unset, [`record`] is a relaxed load and returns.
//! `PHUX_ECHO_FAST=0` turns off the inline PTY write and the small-frame
//! stdout write; the hops stay armed so a before/after uses one binary.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static FAST: OnceLock<bool> = OnceLock::new();
static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();

/// Inline PTY writes and small-frame stdout flushes are on unless
/// `PHUX_ECHO_FAST=0`.
#[must_use]
pub fn fast_path_enabled() -> bool {
    *FAST.get_or_init(|| {
        std::env::var_os("PHUX_ECHO_FAST").as_deref() != Some(std::ffi::OsStr::new("0"))
    })
}

/// Record `hop` if [`PHUX_ECHO_HOPS`](self) is set.
pub fn record(hop: &'static str) {
    let Some(sink) = SINK.get_or_init(open_sink).as_ref() else {
        return;
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut line = [0_u8; 96];
    let written = encode_line(&mut line, hop, nanos);
    if let Ok(mut file) = sink.lock() {
        let _ = file.write_all(&line[..written]);
    }
}

fn open_sink() -> Option<Mutex<std::fs::File>> {
    let path = std::env::var_os("PHUX_ECHO_HOPS")?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    Some(Mutex::new(file))
}

fn encode_line(buf: &mut [u8], hop: &str, nanos: u128) -> usize {
    let pid = std::process::id();
    let text = format!("{pid}\t{hop}\t{nanos}\n");
    let bytes = text.as_bytes();
    let n = bytes.len().min(buf.len());
    buf[..n].copy_from_slice(&bytes[..n]);
    n
}
