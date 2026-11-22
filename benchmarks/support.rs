#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::missing_panics_doc,
    reason = "shared by independently compiled benchmark and gate binaries"
)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "shared source is included as a private module by multiple benchmark/test crate roots"
)]

use std::fmt;
use std::time::Duration;

pub(crate) const FIXED_SEED: u64 = 0x7068_7578_736c_6f67;
pub(crate) const WARMUP_SAMPLES: usize = 12;
pub(crate) const MEASURED_SAMPLES: usize = 80;
pub(crate) const HISTORY_PAGE_LIMIT: usize = 256 * 1024 - 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Corpus {
    Shell80x24,
    Tui200x60,
    Unicode50k,
}

impl Corpus {
    pub(crate) const ALL: [Self; 3] = [Self::Shell80x24, Self::Tui200x60, Self::Unicode50k];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Shell80x24 => "shell-80x24",
            Self::Tui200x60 => "tui-200x60",
            Self::Unicode50k => "unicode-50k",
        }
    }

    pub(crate) const fn geometry(self) -> (u16, u16) {
        match self {
            Self::Shell80x24 => (80, 24),
            Self::Tui200x60 | Self::Unicode50k => (200, 60),
        }
    }

    pub(crate) const fn history_lines(self) -> usize {
        match self {
            Self::Shell80x24 | Self::Tui200x60 => 0,
            Self::Unicode50k => 50_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Comparison {
    AtMost,
    LessThan,
    AtLeast,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Threshold {
    pub(crate) metric: &'static str,
    pub(crate) corpus: Corpus,
    pub(crate) clients: usize,
    pub(crate) comparison: Comparison,
    pub(crate) observed: f64,
    pub(crate) limit: f64,
    pub(crate) unit: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ThresholdFailure(String);

impl fmt::Display for ThresholdFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ThresholdFailure {}

impl Threshold {
    pub(crate) fn check(self) -> Result<(), ThresholdFailure> {
        let passes = match self.comparison {
            Comparison::AtMost => self.observed <= self.limit,
            Comparison::LessThan => self.observed < self.limit,
            Comparison::AtLeast => self.observed >= self.limit,
        };
        if passes {
            return Ok(());
        }
        let relation = match self.comparison {
            Comparison::AtMost => "<=",
            Comparison::LessThan => "<",
            Comparison::AtLeast => ">=",
        };
        Err(ThresholdFailure(format!(
            "threshold failed: metric={} corpus={} clients={} observed={:.3}{} required={} {:.3}{} seed={FIXED_SEED:#x}",
            self.metric,
            self.corpus.label(),
            self.clients,
            self.observed,
            self.unit,
            relation,
            self.limit,
            self.unit,
        )))
    }
}

pub(crate) fn percentile(samples: &mut [Duration], percentile: usize) -> Duration {
    assert!(!samples.is_empty(), "percentile needs at least one sample");
    assert!(
        (1..=100).contains(&percentile),
        "percentile must be 1..=100"
    );
    samples.sort_unstable();
    let rank = samples.len().saturating_mul(percentile).saturating_add(99) / 100;
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

pub(crate) fn deterministic_line(index: usize) -> String {
    let mut state = FIXED_SEED ^ (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    format!("{index:05} λ={state:016x} 東京 e\u{301}cole 🦀 مرحبا\r\n")
}

pub(crate) fn deterministic_page(page: usize, target_bytes: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(target_bytes);
    let mut line = page.saturating_mul(4096);
    while bytes.len() < target_bytes {
        let text = deterministic_line(line);
        let remaining = target_bytes - bytes.len();
        bytes.extend_from_slice(&text.as_bytes()[..text.len().min(remaining)]);
        line = line.saturating_add(1);
    }
    bytes
}
