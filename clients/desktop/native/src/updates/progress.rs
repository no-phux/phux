//! The installer's opt-in archive curl meter and verification phase on stderr.
//! Only recognized records are removed; diagnostics remain available on failure.

use std::io::{self, BufReader, Read};
use std::sync::mpsc::Sender;

use super::StageProgress;

pub(super) fn read_stderr(
    reader: impl Read,
    progress: Option<Sender<StageProgress>>,
) -> io::Result<Vec<u8>> {
    let Some(progress) = progress else {
        let mut bytes = Vec::new();
        BufReader::new(reader).read_to_end(&mut bytes)?;
        return Ok(bytes);
    };
    let mut stderr = Vec::new();
    let mut record = Vec::new();
    let mut last = None;
    for byte in BufReader::new(reader).bytes() {
        let byte = byte?;
        if matches!(byte, b'\r' | b'\n') {
            if !forward(&record, &progress, &mut last) {
                stderr.extend_from_slice(&record);
                stderr.push(byte);
            }
            record.clear();
        } else {
            record.push(byte);
        }
    }
    if !forward(&record, &progress, &mut last) {
        stderr.extend_from_slice(&record);
    }
    Ok(stderr)
}

fn forward(
    record: &[u8],
    progress: &Sender<StageProgress>,
    last: &mut Option<StageProgress>,
) -> bool {
    let Some(next) = parse_record(record) else {
        return false;
    };
    let advances = match (*last, next) {
        (Some(StageProgress::Verifying), _) => false,
        (Some(StageProgress::Downloading(previous)), StageProgress::Downloading(next)) => {
            next > previous
        }
        _ => true,
    };
    if advances {
        let _ = progress.send(next);
        *last = Some(next);
    }
    true
}

fn parse_record(record: &[u8]) -> Option<StageProgress> {
    let text = std::str::from_utf8(record).ok()?.trim();
    if text == "phux-update: verifying" {
        return Some(StageProgress::Verifying);
    }
    curl_percentage(text).map(StageProgress::Downloading)
}

fn curl_percentage(text: &str) -> Option<u8> {
    let value = text.strip_suffix('%')?;
    let (bar, number) = value.rsplit_once(' ').unwrap_or(("", value));
    if !bar.bytes().all(|byte| matches!(byte, b'#' | b' ')) {
        return None;
    }
    decimal_percentage(number)
}

fn decimal_percentage(number: &str) -> Option<u8> {
    let (whole, fraction) = number.split_once('.')?;
    if fraction.len() != 1 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let percent = whole.parse::<u8>().ok()?;
    if percent > 100 || (percent == 100 && fraction != "0") {
        return None;
    }
    Some(percent)
}

#[cfg(test)]
mod tests;
