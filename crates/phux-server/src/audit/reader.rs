//! The read side, shared with `phux audit`: it needs no server.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::Serialize;

use super::writer::rotated;
use super::{AuditRecord, GENESIS, KEEP_ROTATED, LOG_FILE, PUBLIC_KEY_FILE, SCHEMA_VERSION};

/// Every log file in `dir`, oldest first.
#[must_use]
pub fn log_files(dir: &Path) -> Vec<PathBuf> {
    (1..=KEEP_ROTATED)
        .rev()
        .map(|n| rotated(dir, n))
        .chain(std::iter::once(dir.join(LOG_FILE)))
        .filter(|path| path.exists())
        .collect()
}

/// One line that is not a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    /// The file.
    pub file: PathBuf,
    /// 1-based line number.
    pub line: usize,
}

/// Every record in `dir`, oldest first; a line that does not parse is an
/// `Err` in its place.
///
/// # Errors
///
/// A file could not be opened or read.
pub fn read_records(dir: &Path) -> io::Result<Vec<Result<AuditRecord, Unreadable>>> {
    // ponytail: reads every file (at most 5 x 64 MiB); seek from the end if
    // `tail -n` on a full log gets slow.
    let mut out = Vec::new();
    for file in log_files(dir) {
        for (index, line) in BufReader::new(File::open(&file)?).split(b'\n').enumerate() {
            let line = line?;
            if line.is_empty() {
                continue;
            }
            out.push(serde_json::from_slice(&line).map_err(|_| Unreadable {
                file: file.clone(),
                line: index + 1,
            }));
        }
    }
    Ok(out)
}

/// The last `n` records with `seq >= since`, oldest first.
///
/// # Errors
///
/// As [`read_records`]; unreadable lines are skipped.
pub fn tail(dir: &Path, n: usize, since: Option<u64>) -> io::Result<Vec<AuditRecord>> {
    let mut kept = VecDeque::with_capacity(n.min(4096));
    for record in read_records(dir)?.into_iter().flatten() {
        if since.is_some_and(|since| record.seq < since) {
            continue;
        }
        if kept.len() == n {
            kept.pop_front();
        }
        if n > 0 {
            kept.push_back(record);
        }
    }
    Ok(kept.into())
}

/// The result of [`verify_dir`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyReport {
    /// Records that verified.
    pub records: u64,
    /// The first verified record's seq.
    pub first_seq: Option<u64>,
    /// The last verified record's seq.
    pub last_seq: Option<u64>,
    /// Whether the oldest record on disk is the chain's genesis (rotation
    /// may have dropped older ones).
    pub from_genesis: bool,
    /// The first failure, if any.
    pub failure: Option<VerifyFailure>,
}

/// Where and why verification stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyFailure {
    /// The bad record's seq: as written when it parses, else the seq the
    /// chain expected there.
    pub seq: Option<u64>,
    /// The file holding it.
    pub file: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// What did not hold.
    pub reason: String,
}

/// Check every record in `dir`: parse, contiguous `seq`, `prev` links,
/// `hash`, and the signature against `audit-key.pub`. Stops at the first
/// failure.
///
/// # Errors
///
/// A file or the public key could not be read.
pub fn verify_dir(dir: &Path) -> io::Result<VerifyReport> {
    let public = std::fs::read_to_string(dir.join(PUBLIC_KEY_FILE))?;
    let public = hex::decode(public.trim()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{PUBLIC_KEY_FILE} is not hex"),
        )
    })?;
    let key = UnparsedPublicKey::new(&ED25519, public);
    let mut report = VerifyReport {
        records: 0,
        first_seq: None,
        last_seq: None,
        from_genesis: false,
        failure: None,
    };
    let mut prev: Option<String> = None;
    for file in log_files(dir) {
        for (index, line) in BufReader::new(File::open(&file)?).split(b'\n').enumerate() {
            let line = line?;
            if line.is_empty() {
                continue;
            }
            let expected = report.last_seq.map(|seq| seq + 1);
            let fail = |seq, reason: &str| VerifyFailure {
                seq,
                file: file.clone(),
                line: index + 1,
                reason: reason.to_owned(),
            };
            let Ok(record) = serde_json::from_slice::<AuditRecord>(&line) else {
                report.failure = Some(fail(expected, "not a phux.audit/v1 record"));
                return Ok(report);
            };
            let problem = check(&record, expected, prev.as_deref(), &key);
            if let Some(reason) = problem {
                report.failure = Some(fail(Some(record.seq), reason));
                return Ok(report);
            }
            if report.first_seq.is_none() {
                report.first_seq = Some(record.seq);
                report.from_genesis = record.prev == GENESIS;
            }
            report.records += 1;
            report.last_seq = Some(record.seq);
            prev = Some(record.hash);
        }
    }
    Ok(report)
}

fn check(
    record: &AuditRecord,
    expected_seq: Option<u64>,
    prev: Option<&str>,
    key: &UnparsedPublicKey<Vec<u8>>,
) -> Option<&'static str> {
    if record.v != SCHEMA_VERSION {
        return Some("unknown schema version");
    }
    if expected_seq.is_some_and(|seq| seq != record.seq) {
        return Some("seq is not contiguous");
    }
    if prev.is_some_and(|prev| prev != record.prev) {
        return Some("prev does not match the previous record's hash");
    }
    let digest = record.digest();
    if hex::encode(digest) != record.hash {
        return Some("hash does not match the record");
    }
    let Ok(sig) = base64::engine::general_purpose::STANDARD.decode(&record.sig) else {
        return Some("signature is not base64");
    };
    if key.verify(&digest, &sig).is_err() {
        return Some("signature does not verify");
    }
    None
}
