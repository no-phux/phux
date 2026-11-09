//! `phux audit tail|verify`: read this profile's `phux.audit/v1` log
//! straight from disk (`docs/audit.md`); no server is needed.

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use phux_server::audit::{self, AuditRecord};
use usage::Subcommands;

/// Records `tail` prints when `-n` is omitted.
const DEFAULT_TAIL: u32 = 20;

/// Subcommands of `phux audit`.
#[derive(Debug, Subcommands)]
pub(crate) enum AuditAction {
    /// Print the newest records, oldest first.
    Tail {
        /// How many records to print; 20 when omitted.
        #[usage(short = 'n', long, value_name = "NUM")]
        lines: Option<u32>,
        /// Keep printing records as the server appends them.
        #[usage(short, long)]
        follow: bool,
        /// Only records with this seq or later.
        #[usage(long, value_name = "SEQ")]
        since_seq: Option<u64>,
        /// Print each record as its stored JSON line instead of a summary.
        #[usage(long)]
        json: bool,
    },
    /// Check the hash chain and every signature; exits 1 at the first bad
    /// record and names its seq.
    Verify {
        #[usage(flatten)]
        json: super::JsonOpt,
    },
}

pub(crate) fn run(action: AuditAction) -> ExitCode {
    let dir = audit::default_dir();
    match action {
        AuditAction::Tail {
            lines,
            follow,
            since_seq,
            json,
        } => tail(&dir, lines.unwrap_or(DEFAULT_TAIL), follow, since_seq, json),
        AuditAction::Verify { json } => verify(&dir, json.json),
    }
}

fn tail(dir: &Path, lines: u32, follow: bool, since: Option<u64>, json: bool) -> ExitCode {
    let records = match audit::tail(dir, lines as usize, since) {
        Ok(records) => records,
        Err(err) => {
            error!("audit: cannot read {}: {err}", dir.display());
            hint!("the server writes it from its first connection; `phux server` starts one");
            return ExitCode::FAILURE;
        }
    };
    for record in &records {
        print(record, json);
    }
    if follow {
        follow_live(dir, since, json);
    }
    ExitCode::SUCCESS
}

fn print(record: &AuditRecord, json: bool) {
    if json {
        if let Ok(line) = serde_json::to_string(record) {
            outln!("{line}");
        }
        return;
    }
    let actor = record
        .actor
        .credential
        .clone()
        .unwrap_or_else(|| word(&record.actor.transport));
    outln!(
        "{:>6}  {}  {:<18}  {:<8}  {:<22}  {:<24}  {}",
        record.seq,
        record.ts,
        word(&record.kind),
        word(&record.outcome),
        record.action,
        record.target.as_deref().unwrap_or("-"),
        actor,
    );
}

/// The stored spelling of a unit enum (`command.refused`).
fn word(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Poll the live file for appended lines until interrupted, draining the
/// rotated file first when the server rotates under us.
fn follow_live(dir: &Path, since: Option<u64>, json: bool) {
    let live = dir.join(audit::LOG_FILE);
    let stat = |path: &Path| std::fs::metadata(path).map(|meta| (meta.ino(), meta.len()));
    let (mut inode, mut offset) = stat(&live).unwrap_or((0, 0));
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let Ok((now_inode, len)) = stat(&live) else {
            continue;
        };
        if now_inode != inode {
            let _ = read_from(
                &dir.join(format!("{}.1", audit::LOG_FILE)),
                offset,
                since,
                json,
            );
            (inode, offset) = (now_inode, 0);
        }
        if len < offset {
            offset = 0;
        }
        if let Ok(next) = read_from(&live, offset, since, json) {
            offset = next;
        }
    }
}

/// Print the complete lines of `path` after `offset`; the offset after them.
fn read_from(path: &Path, offset: u64, since: Option<u64>, json: bool) -> std::io::Result<u64> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |end| end + 1);
    for line in bytes[..complete].split(|byte| *byte == b'\n') {
        let Ok(record) = serde_json::from_slice::<AuditRecord>(line) else {
            continue;
        };
        if since.is_none_or(|since| record.seq >= since) {
            print(&record, json);
        }
    }
    Ok(offset + complete as u64)
}

fn verify(dir: &Path, json: bool) -> ExitCode {
    let report = match audit::verify_dir(dir) {
        Ok(report) => report,
        Err(err) => {
            let message = format!("cannot verify the audit log in {}: {err}", dir.display());
            if json {
                return crate::commands::json_err::emit(
                    true,
                    &crate::commands::json_err::CliError::new(
                        crate::commands::json_err::codes::IO,
                        message,
                        "the server writes the log and its key on start; `phux doctor` names it",
                    ),
                    1,
                );
            }
            error!("audit: {message}");
            return ExitCode::FAILURE;
        }
    };
    let ok = report.failure.is_none();
    if json {
        let _ = crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "ok": ok,
            "dir": dir,
            "records": report.records,
            "first_seq": report.first_seq,
            "last_seq": report.last_seq,
            "from_genesis": report.from_genesis,
            "failure": report.failure,
        }));
    } else if let Some(failure) = &report.failure {
        error!(
            "audit: verification failed at seq {} ({}:{}): {}",
            failure
                .seq
                .map_or_else(|| "?".to_owned(), |seq| seq.to_string()),
            failure.file.display(),
            failure.line,
            failure.reason,
        );
        if report.records > 0 {
            info!("{} earlier record(s) verified", report.records);
        }
    } else {
        let range = match (report.first_seq, report.last_seq) {
            (Some(first), Some(last)) => format!("seq {first}..={last}"),
            _ => "no records yet".to_owned(),
        };
        let anchor = if report.from_genesis || report.records == 0 {
            ""
        } else {
            "; older records were rotated away"
        };
        ok!(
            "audit: {} record(s) verified ({range}){anchor}",
            report.records
        );
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
