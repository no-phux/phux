//! Orphan reap from the pidfile a previous server left behind.
//!
//! A `SIGKILL` of the server cannot run the in-process grace path. The next
//! start signals a stale group only when the recorded leader pid is still
//! alive and its start time still matches. A dead leader, or a live pid
//! whose start time differs, is pid reuse: log a warning and do not signal.

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
}

#[derive(Debug, Serialize, Deserialize)]
struct PidRecord {
    pid: i32,
    pgid: i32,
    start_time: String,
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
    let record = PidRecord {
        pid,
        pgid: group,
        start_time,
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

/// Signal every stale group under `state_dir` whose leader is still the
/// process we recorded. Refusals are warned and left alone.
pub(super) async fn reap_orphans(state_dir: &Path) {
    let found = read_pidfiles(state_dir);
    let mut kill = Vec::new();
    for orphan in found {
        match decide(orphan.pid, &orphan.start_time) {
            Decision::Kill => kill.push(orphan),
            Decision::Refuse(reason) => {
                tracing::warn!(
                    pid = orphan.pid,
                    pgid = orphan.pgid,
                    "startup orphan: {reason}"
                );
            }
        }
    }
    if kill.is_empty() {
        return;
    }
    let deadline = Instant::now() + STOP_GRACE;
    for orphan in &kill {
        signal_group(orphan.pgid, libc::SIGTERM);
    }
    while Instant::now() < deadline && kill.iter().any(|orphan| process_exists(orphan.pid)) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for orphan in &kill {
        if process_exists(orphan.pid) {
            signal_group(orphan.pgid, libc::SIGKILL);
        }
    }
    let reap_by = Instant::now() + Duration::from_secs(2);
    while Instant::now() < reap_by && kill.iter().any(|orphan| process_exists(orphan.pid)) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for orphan in &kill {
        if !process_exists(orphan.pid) {
            let _ = std::fs::remove_file(&orphan.path);
        }
    }
}

enum Decision {
    Kill,
    Refuse(&'static str),
}

fn decide(pid: i32, recorded: &str) -> Decision {
    if !process_exists(pid) {
        return Decision::Refuse(
            "leader is dead; not signaling its group (the pid may have been reused)",
        );
    }
    match process_start_time(pid) {
        Some(current) if current == recorded => Decision::Kill,
        Some(_) => Decision::Refuse("start time mismatch; not signaling (pid reuse)"),
        None => Decision::Refuse("start time unreadable; not signaling"),
    }
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
    let record: PidRecord = serde_json::from_slice(&bytes).ok()?;
    if record.pid <= 0 || record.pgid <= 1 || record.start_time.is_empty() {
        return None;
    }
    Some(Orphan {
        path: path.to_path_buf(),
        pid: record.pid,
        pgid: record.pgid,
        start_time: record.start_time,
    })
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
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 2 (`comm`) is wrapped in parentheses and may itself contain
    // spaces and parentheses. The last `)` closes it; field 22 is then the
    // 20th whitespace-separated field of what follows (field 3 is index 0).
    let rest = text.rsplit_once(')')?.1;
    let starttime = rest.split_whitespace().nth(19)?;
    Some(starttime.to_owned())
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
    // the size only.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            std::ptr::null_mut(),
            &mut len,
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
            mib.as_mut_ptr(),
            4,
            buf.as_mut_ptr().cast::<libc::c_void>(),
            &mut len,
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn sleep_group() -> std::process::Child {
        use std::os::unix::process::CommandExt as _;
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("1000");
        cmd.process_group(0);
        cmd.spawn().expect("spawn sleep")
    }

    fn group_of(pid: u32) -> i32 {
        // SAFETY: `pid` is a child this test just spawned.
        let group = unsafe { libc::getpgid(i32::try_from(pid).unwrap_or(0)) };
        assert!(group > 1, "sleep should lead a group");
        group
    }

    #[test]
    fn reap_refuses_a_start_time_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = sleep_group();
        let pid = child.id().cast_signed();
        let group = group_of(child.id());
        let real = process_start_time(pid).expect("start time");
        let path = dir
            .path()
            .join("plugin-startup")
            .join("plug")
            .join("web.pid");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = serde_json::to_vec(&PidRecord {
            pid,
            pgid: group,
            start_time: format!("mismatch-{real}"),
        })
        .unwrap();
        std::fs::write(&path, body).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(reap_orphans(dir.path()));
        assert!(
            process_exists(pid),
            "a mismatched start time must not be killed"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn reap_refuses_when_the_leader_is_dead_even_if_the_group_id_is_live() {
        let dir = tempfile::tempdir().unwrap();
        let mut innocent = sleep_group();
        let innocent_group = group_of(innocent.id());
        let mut leader = std::process::Command::new("true").spawn().unwrap();
        let leader_pid = leader.id().cast_signed();
        leader.wait().unwrap();
        assert!(!process_exists(leader_pid));
        let path = dir
            .path()
            .join("plugin-startup")
            .join("plug")
            .join("web.pid");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_vec(&PidRecord {
                pid: leader_pid,
                pgid: innocent_group,
                start_time: "1".to_owned(),
            })
            .unwrap(),
        )
        .unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(reap_orphans(dir.path()));
        assert!(
            process_exists(innocent.id().cast_signed()),
            "a dead leader must not take a live group with it"
        );
        let _ = innocent.kill();
        let _ = innocent.wait();
    }

    #[test]
    fn reap_kills_when_the_leader_and_start_time_match() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = sleep_group();
        let pid = child.id().cast_signed();
        let group = group_of(child.id());
        let path = dir
            .path()
            .join("plugin-startup")
            .join("plug")
            .join("web.pid");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_pidfile(&path, pid, group).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(reap_orphans(dir.path()));
        // The test is the parent, so a killed child is a zombie until waited.
        // `kill(pid, 0)` still succeeds for a zombie; `try_wait` tells them apart.
        let status = child.try_wait().unwrap();
        assert!(
            status.is_some(),
            "a matching leader is reaped (group {group})"
        );
    }
}
