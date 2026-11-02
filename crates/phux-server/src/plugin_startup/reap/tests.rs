#![allow(clippy::expect_used, reason = "tests")]

use std::os::unix::process::CommandExt as _;

use super::*;

fn sleep_group() -> std::process::Child {
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
        server_pid: 0,
        server_start_time: String::new(),
        boot_id: String::new(),
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
            server_pid: 0,
            server_start_time: String::new(),
            boot_id: String::new(),
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
    let start = process_start_time(pid).expect("start time");
    let path = dir
        .path()
        .join("plugin-startup")
        .join("plug")
        .join("web.pid");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // No owner: `write_pidfile` would record this test, and the reap
    // would skip a sidecar whose server is still alive.
    std::fs::write(
        &path,
        serde_json::to_vec(&PidRecord {
            pid,
            pgid: group,
            start_time: start,
            server_pid: 0,
            server_start_time: String::new(),
            boot_id: String::new(),
        })
        .unwrap(),
    )
    .unwrap();

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

#[test]
fn reap_skips_while_the_owning_server_is_alive() {
    let dir = tempfile::tempdir().unwrap();
    let mut sidecar = sleep_group();
    let pid = sidecar.id().cast_signed();
    let group = group_of(sidecar.id());
    let path = dir
        .path()
        .join("plugin-startup")
        .join("plug")
        .join("web.pid");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_pidfile(&path, pid, group).unwrap();
    let record: PidRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let self_pid = unsafe { libc::getpid() };
    assert_eq!(record.server_pid, self_pid);
    assert_eq!(
        record.server_start_time,
        process_start_time(self_pid).expect("server start")
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    rt.block_on(reap_orphans(dir.path()));
    assert!(
        sidecar.try_wait().unwrap().is_none(),
        "a second start must not reap a sidecar whose server is alive"
    );

    let mut owner = sleep_group();
    let owner_pid = owner.id().cast_signed();
    let owner_start = process_start_time(owner_pid).expect("owner start");
    unsafe { libc::kill(owner_pid, libc::SIGKILL) };
    owner.wait().unwrap();
    assert!(!process_exists(owner_pid));
    let dead_owner = PidRecord {
        server_pid: owner_pid,
        server_start_time: owner_start,
        ..record
    };
    std::fs::write(&path, serde_json::to_vec(&dead_owner).unwrap()).unwrap();
    rt.block_on(reap_orphans(dir.path()));
    assert!(
        sidecar.try_wait().unwrap().is_some(),
        "once the owning server is gone the sidecar is reaped"
    );
}

#[test]
fn reap_kills_a_grandchild_after_the_leader_exits() {
    let dir = tempfile::tempdir().unwrap();
    let leader_path = dir.path().join("leader");
    let grand_path = dir.path().join("grand");
    let script = format!(
        "echo $$ > '{}'; sleep 1000 & echo $! > '{}'; wait\n",
        leader_path.display(),
        grand_path.display()
    );
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg("-c").arg(&script);
    cmd.process_group(0);
    let mut leader = cmd.spawn().expect("spawn leader");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !grand_path.is_file() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let leader_pid: i32 = std::fs::read_to_string(&leader_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(leader_pid, leader.id().cast_signed());
    let start = process_start_time(leader_pid).expect("leader start");
    let group = group_of(leader.id());
    assert_eq!(group, leader_pid, "the shell should lead its group");
    let grand: i32 = std::fs::read_to_string(&grand_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let grand_start = process_start_time(grand).expect("grandchild start");
    assert!(
        start_is_not_before(&grand_start, &start),
        "grandchild start {grand_start} should be after leader start {start}"
    );
    unsafe { libc::kill(leader_pid, libc::SIGKILL) };
    leader.wait().unwrap();
    assert!(!process_exists(leader_pid));
    assert!(
        process_exists(grand),
        "grandchild should outlive the leader"
    );

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
            pgid: group,
            start_time: start,
            server_pid: 0,
            server_start_time: String::new(),
            boot_id: String::new(),
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
        !process_exists(grand),
        "a dead leader's later group member is reaped"
    );
}

#[test]
fn reap_refuses_a_live_leader_whose_pgid_is_not_its_pid() {
    let dir = tempfile::tempdir().unwrap();
    let mut leader = sleep_group();
    let leader_pid = leader.id().cast_signed();
    let start = process_start_time(leader_pid).expect("leader start");
    let mut innocent = sleep_group();
    let innocent_group = group_of(innocent.id());
    assert_ne!(innocent_group, leader_pid);
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
            start_time: start,
            server_pid: 0,
            server_start_time: String::new(),
            boot_id: String::new(),
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
        innocent.try_wait().unwrap().is_none(),
        "a pidfile whose pgid is not the leader must not signal that group"
    );
    assert!(leader.try_wait().unwrap().is_none());
    let _ = leader.kill();
    let _ = leader.wait();
    let _ = innocent.kill();
    let _ = innocent.wait();
}

#[test]
fn reap_skips_when_the_owner_start_time_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let mut sidecar = sleep_group();
    let pid = sidecar.id().cast_signed();
    let group = group_of(sidecar.id());
    let start = process_start_time(pid).expect("start");
    let path = dir
        .path()
        .join("plugin-startup")
        .join("plug")
        .join("web.pid");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // SAFETY: the test process is the recorded owner.
    let server_pid = unsafe { libc::getpid() };
    std::fs::write(
        &path,
        serde_json::to_vec(&PidRecord {
            pid,
            pgid: group,
            start_time: start,
            server_pid,
            server_start_time: String::new(),
            boot_id: current_boot_id().unwrap_or_default(),
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
        sidecar.try_wait().unwrap().is_none(),
        "an unreadable owner start time must fail toward alive"
    );
    let _ = sidecar.kill();
    let _ = sidecar.wait();
}

#[test]
fn reap_does_not_kill_a_pidfile_from_another_boot() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = sleep_group();
    let pid = child.id().cast_signed();
    let group = group_of(child.id());
    let start = process_start_time(pid).expect("start");
    let path = dir
        .path()
        .join("plugin-startup")
        .join("plug")
        .join("web.pid");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::to_vec(&PidRecord {
            pid,
            pgid: group,
            start_time: start,
            server_pid: 0,
            server_start_time: String::new(),
            boot_id: "not-this-boot".to_owned(),
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
        child.try_wait().unwrap().is_none(),
        "a pidfile from another boot must not be signaled"
    );
    assert!(!path.exists(), "a stale boot pidfile is removed");
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn frozen_pidfile() {
    phux_frozen::json::<PidRecord>("plugin-pidfile").check();
}
