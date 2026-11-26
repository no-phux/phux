//! OSC 7577 program resume (spec rev 0.5).
//!
//! The MAC and the time are checked before any other key is decoded. A failure
//! of either is ignored and leaves the record alone. A report that passes both
//! and then breaks the grammar is rejected, which removes that owner's record.
//! OSC 133, RIS, DECSTR, and the alternate screen are not reports and do not
//! clear anything.

use std::collections::HashMap;

use base64::Engine as _;
use phux_core::ProgramResume;
use sha2::{Digest, Sha256};

use super::osc_scan::OscScan;

/// What follows `ESC ]` in a report.
const INTRO: &[u8] = b"7577;";
/// Largest body that still keeps a BEL-terminated sequence within 2048 bytes.
const MAX_BODY: usize = 2040;
/// Acceptance window around the terminal clock, in seconds.
const WINDOW_SECS: u64 = 60;
/// The spec's test token.
#[cfg(test)]
const DUMMY_TOKEN: &str = "dummy-token-replace-me";

const VALUE_EXTRA: &[u8] = b"_.,+/=-";

/// What one finished body asks for, after the MAC and the time have passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// No effect. A bad MAC, a bad time, or a sequence that was not whole.
    Ignored,
    /// The report was verified and then refused. `pid` is the owner whose
    /// records go away; `None` means the report named no usable owner.
    Rejected { pid: Option<u32> },
    /// Replace the pane's record with this command.
    Accepted(Parsed),
    /// `clear=1` for this owner.
    Cleared { pid: u32 },
}

impl Outcome {
    /// `ignored` or `rejected`, for the spec's invalid-report table.
    #[cfg(test)]
    pub(super) const fn label(&self) -> &'static str {
        match self {
            Self::Ignored => "ignored",
            Self::Rejected { .. } => "rejected",
            Self::Accepted(_) | Self::Cleared { .. } => "accepted",
        }
    }
}

/// A report that passed the MAC, the time, and the grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Parsed {
    pub(super) pid: u32,
    pub(super) time: u64,
    pub(super) argv: Vec<String>,
    pub(super) cwd: Option<String>,
}

pub(super) use phux_core::ProgramResumeUpdate as Update;

/// The scanner and the last time accepted for each pid.
pub(super) struct ProgramResumeScan {
    osc: OscScan,
    token: Option<String>,
    last_time: HashMap<u32, u64>,
    /// When set, tests name the reporter instead of asking the kernel.
    trust_override: Option<bool>,
    sink: Option<tokio::sync::mpsc::UnboundedSender<Update>>,
}

impl Default for ProgramResumeScan {
    fn default() -> Self {
        Self {
            osc: OscScan::new(INTRO, MAX_BODY),
            token: None,
            last_time: HashMap::new(),
            trust_override: None,
            sink: None,
        }
    }
}

impl ProgramResumeScan {
    pub(super) fn subscribe(&mut self) -> tokio::sync::mpsc::UnboundedReceiver<Update> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.sink = Some(tx);
        rx
    }

    pub(super) fn set_token(&mut self, token: String) {
        self.token = Some(token);
    }

    pub(super) fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Remember a time already accepted, so a replay after an upgrade is not newer.
    pub(super) fn seed_clock(&mut self, pid: u32, time: u64) {
        self.last_time.insert(pid, time);
    }

    #[cfg(test)]
    pub(super) const fn set_trust_override(&mut self, trusted: Option<bool>) {
        self.trust_override = trusted;
    }

    /// Scan `bytes`. `trusted` is the kernel's reporter answer; a test override wins.
    pub(super) fn observe(&mut self, bytes: &[u8], now: u64, trusted: bool) {
        let Some(token) = self.token.clone() else {
            return;
        };
        let trusted = self.trust_override.unwrap_or(trusted);
        let mut last = std::mem::take(&mut self.last_time);
        let mut updates = Vec::new();
        self.osc.scan(bytes, |body| {
            if let Some(update) = apply(
                classify(body, token.as_bytes(), now, &last),
                trusted,
                &mut last,
            ) {
                updates.push(update);
            }
        });
        self.last_time = last;
        if let Some(sink) = &self.sink {
            for update in updates {
                let _ = sink.send(update);
            }
        }
    }
}

/// Apply one classified report. The monotonic clock keeps accepted times so an
/// older replay cannot replace a later report.
fn apply(outcome: Outcome, trusted: bool, last: &mut HashMap<u32, u64>) -> Option<Update> {
    match outcome {
        Outcome::Ignored | Outcome::Rejected { pid: None } => None,
        Outcome::Rejected { pid: Some(pid) } | Outcome::Cleared { pid } => {
            Some(Update::Clear { pid })
        }
        Outcome::Accepted(parsed) => {
            last.insert(parsed.pid, parsed.time);
            Some(Update::Put(record_from(parsed, trusted)))
        }
    }
}

fn record_from(parsed: Parsed, trusted: bool) -> ProgramResume {
    let facts = owner_facts(parsed.pid);
    let cwd = parsed.cwd.map(std::path::PathBuf::from).or(facts.cwd);
    let uid = facts.uid;
    ProgramResume {
        argv: parsed.argv,
        cwd,
        pid: parsed.pid,
        started: facts.started,
        uid,
        time: parsed.time,
        tried: false,
        trusted: trusted && uid != 0,
    }
}

/// Classify `body` against `token` at `now`. The MAC and the time are decided
/// before argv or cwd is decoded.
pub(super) fn classify(body: &[u8], token: &[u8], now: u64, last: &HashMap<u32, u64>) -> Outcome {
    if body == b"?" {
        return Outcome::Ignored;
    }
    let Some(prefix) = mac_prefix(body) else {
        return Outcome::Ignored;
    };
    let (head, hex) = prefix;
    if !mac_matches(head, token, hex) {
        return Outcome::Ignored;
    }
    let Some(time) = time_of(head) else {
        return Outcome::Ignored;
    };
    if time_outside(time, now) {
        return Outcome::Ignored;
    }
    let pid_for_clock = well_formed_pid(head);
    if let Some(pid) = pid_for_clock
        && last.get(&pid).is_some_and(|previous| time < *previous)
    {
        return Outcome::Ignored;
    }
    match parse_pairs(head) {
        Err(pid) => Outcome::Rejected { pid },
        Ok(parsed) => {
            if parsed.clear {
                Outcome::Cleared { pid: parsed.pid }
            } else {
                Outcome::Accepted(Parsed {
                    pid: parsed.pid,
                    time,
                    argv: parsed.argv,
                    cwd: parsed.cwd,
                })
            }
        }
    }
}

/// Bytes before the final `:mac=`, and the 64 hex digits, when the body ends
/// with that key and nothing else.
fn mac_prefix(body: &[u8]) -> Option<(&[u8], &[u8])> {
    const MARK: &[u8] = b":mac=";
    if body.len() < MARK.len() + 64 {
        return None;
    }
    let (head, hex) = body.split_at(body.len() - 64);
    let head = head.strip_suffix(MARK)?;
    hex.iter()
        .all(u8::is_ascii_hexdigit)
        .then_some((head, hex))
        .filter(|(_, hex)| {
            hex.iter()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
}

fn mac_matches(head: &[u8], token: &[u8], hex: &[u8]) -> bool {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hasher = Sha256::new();
    hasher.update(head);
    hasher.update(b":");
    hasher.update(token);
    let digest = hasher.finalize();
    let mut encoded = [0u8; 64];
    for (index, byte) in digest.iter().enumerate() {
        encoded[index * 2] = HEX[(byte >> 4) as usize];
        encoded[index * 2 + 1] = HEX[(byte & 0xf) as usize];
    }
    hex == encoded.as_slice()
}

fn time_of(head: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(head).ok()?;
    let mut found = None;
    for pair in text.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key == "time" {
            found = parse_uint(value);
        }
    }
    found
}

fn well_formed_pid(head: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(head).ok()?;
    let mut found = None;
    let mut count = 0u32;
    for pair in text.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key == "pid" {
            count += 1;
            found = parse_uint(value).and_then(|value| u32::try_from(value).ok());
        }
    }
    (count == 1).then_some(found).flatten()
}

fn parse_uint(value: &str) -> Option<u64> {
    if value.is_empty() || !value.starts_with(|ch: char| ch.is_ascii_digit() && ch != '0') {
        return None;
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn time_outside(time: u64, now: u64) -> bool {
    let time = i128::from(time);
    let now = i128::from(now);
    (time - now).abs() > i128::from(WINDOW_SECS)
}

struct Pairs {
    pid: u32,
    clear: bool,
    argv: Vec<String>,
    cwd: Option<String>,
}

fn parse_pairs(head: &[u8]) -> Result<Pairs, Option<u32>> {
    let text = std::str::from_utf8(head).map_err(|_| None)?;
    if text.is_empty() {
        return Err(None);
    }
    let mut seen = Vec::new();
    let mut pid = None;
    let mut argv = None;
    let mut cwd = None;
    let mut clear = false;
    let mut need = Vec::new();
    let mut saw_clear = false;
    let mut saw_argv = false;
    for pair in text.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(pid);
        };
        if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_lowercase()) {
            return Err(pid);
        }
        if !value_chars(value) {
            return Err(pid);
        }
        if seen.contains(&key) {
            return Err(pid);
        }
        seen.push(key);
        match key {
            "pid" => pid = Some(parse_pid(value).ok_or(None)?),
            "time" => {
                parse_uint(value).ok_or(pid)?;
            }
            "argv" => {
                saw_argv = true;
                argv = Some(decode_argv(value).map_err(|()| pid)?);
            }
            "cwd" => cwd = Some(decode_cwd(value).map_err(|()| pid)?),
            "clear" => {
                if value != "1" {
                    return Err(pid);
                }
                saw_clear = true;
                clear = true;
            }
            "need" => {
                need = value
                    .split(',')
                    .filter(|key| !key.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            "mac" => return Err(pid),
            _ => {}
        }
    }
    let Some(pid) = pid else {
        return Err(None);
    };
    if saw_argv == saw_clear {
        return Err(Some(pid));
    }
    for key in &need {
        if !implemented(key) {
            return Err(Some(pid));
        }
    }
    if clear {
        return Ok(Pairs {
            pid,
            clear: true,
            argv: Vec::new(),
            cwd: None,
        });
    }
    Ok(Pairs {
        pid,
        clear: false,
        argv: argv.ok_or(Some(pid))?,
        cwd,
    })
}

fn parse_pid(value: &str) -> Option<u32> {
    let time = parse_uint(value)?;
    u32::try_from(time).ok()
}

fn value_chars(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || VALUE_EXTRA.contains(&byte))
}

fn implemented(key: &str) -> bool {
    matches!(
        key,
        "pid" | "time" | "argv" | "cwd" | "clear" | "need" | "mac"
    )
}

fn decode_argv(value: &str) -> Result<Vec<String>, ()> {
    if value.is_empty() {
        return Err(());
    }
    let mut argv = Vec::new();
    for element in value.split(',') {
        argv.push(decode_text(element)?);
    }
    let program = argv.first().ok_or(())?;
    if !valid_program(program) {
        return Err(());
    }
    Ok(argv)
}

fn decode_cwd(value: &str) -> Result<String, ()> {
    let path = decode_text(value)?;
    if path.starts_with('/') && path.len() > 1 {
        Ok(path)
    } else {
        Err(())
    }
}

fn valid_program(program: &str) -> bool {
    if program.is_empty() {
        return false;
    }
    if program.starts_with('/') {
        return program.len() > 1;
    }
    !program.starts_with('-') && !program.contains('/')
}

fn decode_text(value: &str) -> Result<String, ()> {
    if value.is_empty() {
        return Ok(String::new());
    }
    let bytes = if value.contains('=') {
        if !value.len().is_multiple_of(4) || value.chars().filter(|ch| *ch == '=').count() > 2 {
            return Err(());
        }
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(|_| ())?
    } else {
        base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(value)
            .map_err(|_| ())?
    };
    let text = String::from_utf8(bytes).map_err(|_| ())?;
    if text.chars().any(is_control) {
        return Err(());
    }
    Ok(text)
}

const fn is_control(ch: char) -> bool {
    matches!(ch, '\u{0000}'..='\u{001F}' | '\u{007F}' | '\u{0080}'..='\u{009F}')
}

struct OwnerFacts {
    uid: u32,
    started: u64,
    cwd: Option<std::path::PathBuf>,
}

fn owner_facts(pid: u32) -> OwnerFacts {
    let our = rustix::process::geteuid().as_raw();
    let Some(pid_i) = i32::try_from(pid).ok() else {
        return OwnerFacts {
            uid: our,
            started: 0,
            cwd: None,
        };
    };
    OwnerFacts {
        uid: parent_and_uid(pid_i).map_or(our, |(_, uid)| uid),
        started: phux_resource::proc_query::process_start_time(pid_i).unwrap_or(0),
        cwd: phux_resource::cwd_query::process_cwd(pid),
    }
}

/// Whether the foreground process of `master` is a same-uid direct child of `shell`.
pub(super) fn reporter_trusted(shell_pid: i32, master_fd: std::os::fd::RawFd) -> bool {
    let Some(leader) = phux_resource::proc_query::foreground_pgid(master_fd) else {
        return false;
    };
    let Some((parent, uid)) = parent_and_uid(leader) else {
        return false;
    };
    direct_child(shell_pid, parent, uid, rustix::process::geteuid().as_raw())
}

/// The reporter check, with the kernel answers supplied by the caller.
pub(super) const fn direct_child(shell_pid: i32, parent_pid: i32, uid: u32, our_uid: u32) -> bool {
    shell_pid > 0 && parent_pid == shell_pid && uid == our_uid && our_uid != 0
}

fn parent_and_uid(pid: i32) -> Option<(i32, u32)> {
    if pid <= 0 {
        return None;
    }
    platform_parent_uid(pid)
}

#[cfg(target_os = "linux")]
fn platform_parent_uid(pid: i32) -> Option<(i32, u32)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut parent = None;
    let mut uid = None;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            parent = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("Uid:") {
            uid = rest
                .split_whitespace()
                .next()
                .and_then(|word| word.parse().ok());
        }
    }
    Some((parent?, uid?))
}

#[cfg(target_os = "macos")]
fn platform_parent_uid(pid: i32) -> Option<(i32, u32)> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::addr_of_mut!(info).cast(),
            size,
        )
    };
    if written < size {
        return None;
    }
    let parent = i32::try_from(info.pbi_ppid).ok()?;
    let uid = u32::try_from(info.pbi_uid).ok()?;
    Some((parent, uid))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_parent_uid(_pid: i32) -> Option<(i32, u32)> {
    None
}

/// SHA-256 of `head`, a colon, and `token`, as 64 lowercase hex digits.
#[cfg(test)]
fn mac_hex(head: &str, token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(head.as_bytes());
    hasher.update(b":");
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0xf) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests;
