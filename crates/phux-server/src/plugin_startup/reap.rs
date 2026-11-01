//! Orphan reap from the pidfile a previous server left behind.
//!
//! A `SIGKILL` of the server cannot run the in-process grace path. The
//! pidfile names the sidecar and the server that owned it. A start skips
//! the group while that server is still alive. When the owner is gone, it
//! signals the group if the leader pid is alive and its start time matches,
//! or if the leader is already dead but the group still has a member that
//! started after the recorded spawn (a grandchild whose leader honored
//! `SIGTERM` from `PDEATHSIG`). A mismatched start time is pid reuse: the
//! pidfile is removed and the group is not signaled.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use phux_plugin::startup_log;
use serde::{Deserialize, Serialize};

use super::stop::STOP_GRACE;

/// One pidfile the previous process wrote.
#[derive(Debug)]
struct Orphan {
    path: PathBuf,
    pid: i32,
    pgid: i32,
    start_time: String,
    server_pid: i32,
    server_start_time: String,
    boot_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PidRecord {
    pid: i32,
    pgid: i32,
    start_time: String,
    /// Pid of the server that spawned this sidecar. `0` when unknown
    /// (a pidfile written before owners were recorded).
    #[serde(default)]
    server_pid: i32,
    /// Start time of [`Self::server_pid`], same encoding as `start_time`.
    #[serde(default)]
    server_start_time: String,
    /// Boot id captured when the pidfile was written. Empty on older
    /// pidfiles. A different boot means start-time ticks are not comparable.
    #[serde(default)]
    boot_id: String,
}

/// Write the pidfile the next start reads. The leader's start time is
/// recorded now, while the pid still names the process we spawned.
pub(super) fn write_pidfile(path: &Path, pid: i32, group: i32) -> std::io::Result<()> {
    let Some(start_time) = process_start_time(pid) else {
        tracing::warn!(
            pid,
            "startup: could not read the leader start time; not writing a pidfile"
        );
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        super::spawn::create_dir_private(parent)?;
    }
    // SAFETY: `getpid` reads this process's pid.
    let server_pid = unsafe { libc::getpid() };
    let server_start_time = process_start_time(server_pid).unwrap_or_default();
    let record = PidRecord {
        pid,
        pgid: group,
        start_time,
        server_pid,
        server_start_time,
        boot_id: current_boot_id().unwrap_or_default(),
    };
    let body = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("pid.tmp");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(startup_log::LOG_FILE_MODE)
            .open(&tmp)?;
        file.write_all(&body)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Signal every stale group under `state_dir` whose owner server is gone
/// and whose recorded group is still the one we spawned. A refusal removes
/// the pidfile so the next start does not warn again.
pub(super) async fn reap_orphans(state_dir: &Path) {
    let boot = current_boot_id();
    let found = read_pidfiles(state_dir);
    let mut kill = Vec::new();
    for orphan in found {
        match decide(&orphan, boot.as_deref()) {
            Decision::Kill => kill.push(orphan),
            Decision::Skip => {}
            Decision::Drop(reason) => drop_orphan(&orphan, reason),
        }
    }
    if kill.is_empty() {
        return;
    }
    let deadline = Instant::now() + STOP_GRACE;
    for orphan in &kill {
        signal_group(orphan.pgid, libc::SIGTERM);
    }
    while Instant::now() < deadline && kill.iter().any(still_present) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for orphan in &kill {
        // The group can exit, or its pid can be reused, during the grace.
        // Decide again before `SIGKILL` so a reused pgid is not signaled.
        match decide(orphan, boot.as_deref()) {
            Decision::Kill if still_present(orphan) => {
                signal_group(orphan.pgid, libc::SIGKILL);
            }
            Decision::Drop(reason) => drop_orphan(orphan, reason),
            Decision::Kill | Decision::Skip => {}
        }
    }
    let reap_by = Instant::now() + Duration::from_secs(2);
    while Instant::now() < reap_by && kill.iter().any(still_present) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for orphan in &kill {
        if !still_present(orphan) {
            let _ = std::fs::remove_file(&orphan.path);
        }
    }
}

fn drop_orphan(orphan: &Orphan, reason: &str) {
    tracing::warn!(
        pid = orphan.pid,
        pgid = orphan.pgid,
        "startup orphan: {reason}"
    );
    let _ = std::fs::remove_file(&orphan.path);
}

enum Decision {
    /// Owner server is gone and the recorded group is still ours.
    Kill,
    /// The owning server is still the process that wrote the pidfile.
    Skip,
    /// Remove the pidfile. Signaling would hit the wrong process, or the
    /// sidecar has already exited.
    Drop(&'static str),
}

fn decide(orphan: &Orphan, boot: Option<&str>) -> Decision {
    if boot_is_stale(&orphan.boot_id, boot) {
        return Decision::Drop("pidfile is from a different boot; not signaling");
    }
    if owner_alive(orphan) {
        return Decision::Skip;
    }
    // The leader is `process_group(0)`, so its pid and pgid are the same.
    // A file that disagrees is corrupt; signaling `pgid` could hit a
    // different live group.
    if orphan.pgid != orphan.pid {
        return Decision::Drop("pgid is not the leader; not signaling");
    }
    if process_exists(orphan.pid) {
        return match process_start_time(orphan.pid) {
            Some(current) if current == orphan.start_time => Decision::Kill,
            Some(_) => Decision::Drop("start time mismatch; not signaling (pid reuse)"),
            None => Decision::Drop("start time unreadable; not signaling"),
        };
    }
    if group_has_later_member(orphan.pgid, &orphan.start_time) {
        return Decision::Kill;
    }
    Decision::Drop("leader is dead and its group has no later member; not signaling")
}

fn owner_alive(orphan: &Orphan) -> bool {
    if orphan.server_pid <= 0 {
        return false;
    }
    if !process_exists(orphan.server_pid) {
        return false;
    }
    // An unreadable start time fails toward "alive". An empty recorded
    // start time is accepted on purpose: the owner pid was alive when the
    // pidfile was written, but its start time could not be read. While
    // that pid still exists the reap is skipped. A reused pid whose start
    // time is readable and different is not the owner.
    match process_start_time(orphan.server_pid) {
        Some(_) if orphan.server_start_time.is_empty() => true,
        Some(current) => current == orphan.server_start_time,
        None => true,
    }
}

/// Start-time ticks reset on reboot. A pidfile from another boot is stale
/// even when the pid numbers match. An empty recorded id is an older file
/// and is judged by the start time instead. An unreadable current boot id
/// fails toward not signaling.
fn boot_is_stale(recorded: &str, current: Option<&str>) -> bool {
    if recorded.is_empty() {
        return false;
    }
    current.is_none_or(|current| current != recorded)
}

/// The recorded leader is still that process, or the group still has a
/// member started after the recorded spawn.
fn still_present(orphan: &Orphan) -> bool {
    if process_exists(orphan.pid) {
        return process_start_time(orphan.pid).is_none_or(|current| current == orphan.start_time);
    }
    group_has_later_member(orphan.pgid, &orphan.start_time)
}

fn read_pidfiles(state_dir: &Path) -> Vec<Orphan> {
    let root = state_dir.join(startup_log::DIR_NAME);
    let Ok(plugins) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for plugin in plugins.flatten() {
        let Ok(files) = std::fs::read_dir(plugin.path()) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name();
            let label = name.to_string_lossy();
            // A killed server can leave the temp file from `write_pidfile`.
            if label.ends_with(".pid.tmp") {
                let _ = std::fs::remove_file(file.path());
                continue;
            }
            if !Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pid"))
            {
                continue;
            }
            if let Some(orphan) = read_one(&file.path()) {
                found.push(orphan);
            }
        }
    }
    found
}

fn read_one(path: &Path) -> Option<Orphan> {
    let bytes = std::fs::read(path).ok()?;
    let Ok(record) = serde_json::from_slice::<PidRecord>(&bytes) else {
        let _ = std::fs::remove_file(path);
        return None;
    };
    if record.pid <= 0 || record.pgid <= 1 || record.start_time.is_empty() {
        let _ = std::fs::remove_file(path);
        return None;
    }
    Some(Orphan {
        path: path.to_path_buf(),
        pid: record.pid,
        pgid: record.pgid,
        start_time: record.start_time,
        server_pid: record.server_pid,
        server_start_time: record.server_start_time,
        boot_id: record.boot_id,
    })
}

/// `true` when some process other than the recorded leader is in `pgid`
/// and started after `recorded`. That is a grandchild the leader left
/// behind. A member that started earlier is not this spawn.
fn group_has_later_member(pgid: i32, recorded: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux_group_has_later_member(pgid, recorded)
    }
    #[cfg(target_os = "macos")]
    {
        macos_group_has_later_member(pgid, recorded)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (pgid, recorded);
        false
    }
}

#[cfg(target_os = "linux")]
fn linux_group_has_later_member(pgid: i32, recorded: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(member) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if member <= 0 || member == pgid {
            continue;
        }
        let Some((pgrp, start)) = linux_pgrp_and_start(member) else {
            continue;
        };
        if pgrp == pgid && start_is_not_before(&start, recorded) {
            return true;
        }
    }
    false
}

#[cfg(target_os = "macos")]
fn macos_group_has_later_member(pgid: i32, recorded: &str) -> bool {
    // `proc_listpgrppids` returns a byte count. A null buffer asks for the
    // size; divide by `pid_t` to get a pid count.
    let needed = unsafe { libc::proc_listpgrppids(pgid, std::ptr::null_mut(), 0) };
    let count = pid_count_from_bytes(needed);
    if count == 0 {
        return false;
    }
    let mut pids = vec![0_i32; count];
    let bytes = i32::try_from(pids.len() * std::mem::size_of::<libc::pid_t>()).unwrap_or(i32::MAX);
    let got = unsafe { libc::proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), bytes) };
    let count = pid_count_from_bytes(got).min(pids.len());
    if count == 0 {
        return false;
    }
    for pid in pids.into_iter().take(count) {
        if pid <= 0 || pid == pgid {
            continue;
        }
        let Some(start) = process_start_time(pid) else {
            continue;
        };
        if start_is_not_before(&start, recorded) {
            return true;
        }
    }
    false
}

/// `proc_listpgrppids` reports bytes. Zero and errors are an empty list.
#[cfg(target_os = "macos")]
fn pid_count_from_bytes(bytes: i32) -> usize {
    let Ok(bytes) = usize::try_from(bytes) else {
        return 0;
    };
    bytes / std::mem::size_of::<libc::pid_t>()
}

/// Linux start times are clock ticks (10ms). macOS start times are
/// `sec.usec`. A grandchild forked in the leader's own tick shares that
/// value, so the member counts when it is not strictly earlier. The leader
/// pid itself is excluded by the caller.
fn start_is_not_before(member: &str, recorded: &str) -> bool {
    fn parts(value: &str) -> Option<(u64, u64)> {
        match value.split_once('.') {
            Some((sec, usec)) => Some((sec.parse().ok()?, usec.parse().ok()?)),
            None => Some((value.parse().ok()?, 0)),
        }
    }
    match (parts(member), parts(recorded)) {
        (Some(member), Some(recorded)) => member >= recorded,
        _ => false,
    }
}

/// `true` when `pid` exists. `EPERM` still counts: the process is there.
pub(super) fn process_exists(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill(pid, 0)` only probes existence. It does not signal.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// `true` when `pgid` still has a member. `killpg(pgid, 0)` returns
/// `ESRCH` once the group is empty. `EPERM` still counts as present.
pub(super) fn group_exists(pgid: i32) -> bool {
    if pgid <= 1 {
        return false;
    }
    // SAFETY: signal 0 does not deliver a signal. It only probes the group.
    let rc = unsafe { libc::killpg(pgid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Signal `pgid`, never this process's own group and never `0` or `1`.
pub(super) fn signal_group(pgid: i32, signal: i32) {
    if pgid <= 1 {
        return;
    }
    // SAFETY: `getpgrp` reads this process's group. The comparison is so a
    // corrupt pidfile cannot signal the server itself.
    let ours = unsafe { libc::getpgrp() };
    if pgid == ours {
        tracing::warn!(
            pgid,
            "startup: refusing to signal the server's own process group"
        );
        return;
    }
    // SAFETY: `pgid` is a group we recorded for a sidecar we spawned, or a
    // pidfile we have just checked against the leader's start time. `killpg`
    // fails with `ESRCH` when the group is already gone.
    unsafe {
        libc::killpg(pgid, signal);
    }
}

/// Linux: `/proc/<pid>/stat` field 22 (`starttime`). macOS: `sysctl`
/// `KERN_PROC_PID`, the `timeval` at the start of `kp_proc` (`p_starttime`).
pub(super) fn process_start_time(pid: i32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        linux_start_time(pid)
    }
    #[cfg(target_os = "macos")]
    {
        macos_start_time(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "linux")]
fn linux_start_time(pid: i32) -> Option<String> {
    linux_pgrp_and_start(pid).map(|(_, start)| start)
}

/// Field 5 (`pgrp`) and field 22 (`starttime`) of `/proc/<pid>/stat`.
/// Field 2 (`comm`) is wrapped in parentheses and may itself contain
/// spaces and parentheses. The last `)` closes it; field 3 is then index 0.
#[cfg(target_os = "linux")]
fn linux_pgrp_and_start(pid: i32) -> Option<(i32, String)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = text.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let pgrp: i32 = fields.nth(2)?.parse().ok()?;
    // `nth(2)` consumed field 5 (index 2). Field 22 is 17 fields later.
    let starttime = fields.nth(16)?;
    Some((pgrp, starttime.to_owned()))
}

#[cfg(target_os = "macos")]
fn macos_start_time(pid: i32) -> Option<String> {
    // `kinfo_proc` begins with `kp_proc`, and `kp_proc.p_starttime` is a
    // `timeval` at offset 0 of that struct (it aliases the first union arm).
    // libc does not bind `kinfo_proc` on macOS, so the sysctl result's first
    // two words are the start time the review requires.
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    let mut len: libc::size_t = 0;
    // SAFETY: the mib names a kernel query. A null output buffer asks for
    // the size only. `&raw mut` does not create a reference.
    let rc = unsafe {
        libc::sysctl(
            &raw mut mib[0],
            4,
            std::ptr::null_mut(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len < 16 {
        return None;
    }
    let mut buf = vec![0_u8; len];
    // SAFETY: `buf` is `len` bytes, which the previous call reported, and
    // the mib still names `KERN_PROC_PID` for `pid`.
    let rc = unsafe {
        libc::sysctl(
            &raw mut mib[0],
            4,
            (&raw mut buf[0]).cast::<libc::c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len < 16 {
        return None;
    }
    let sec = i64::from_ne_bytes(buf.get(0..8)?.try_into().ok()?);
    let usec = i32::from_ne_bytes(buf.get(8..12)?.try_into().ok()?);
    Some(format!("{sec}.{usec}"))
}

/// Linux: `/proc/sys/kernel/random/boot_id`. macOS: `kern.boottime`.
///
/// `kern.boottime` follows the wall clock, so setting the clock changes
/// the id. [`boot_is_stale`] then skips the reap instead of signaling.
/// That is fail-safe: a shifted clock does not kill a live group.
fn current_boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }
    #[cfg(target_os = "macos")]
    {
        macos_boot_time()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// `kern.boottime` moves when the wall clock is set. Callers treat a
/// mismatch as a different boot and skip the reap.
#[cfg(target_os = "macos")]
fn macos_boot_time() -> Option<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    let mut tv = std::mem::MaybeUninit::<libc::timeval>::zeroed();
    let mut len: libc::size_t = std::mem::size_of::<libc::timeval>();
    // SAFETY: the mib names `kern.boottime`. `tv` is a zeroed `timeval`
    // and `len` is its size. `&raw mut` does not create a reference.
    let rc = unsafe {
        libc::sysctl(
            &raw mut mib[0],
            2,
            tv.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: sysctl wrote a `timeval` or we returned above.
    let tv = unsafe { tv.assume_init() };
    Some(format!("{}.{}", tv.tv_sec, tv.tv_usec))
}

#[cfg(test)]
mod tests;
