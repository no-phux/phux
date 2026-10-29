//! Real PTY requested-exec classification, not launcher exit-code inference.
//! Also the trampoline cache and non-strict fallback decision.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::OnceLock;

struct Spawned {
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
}
impl Spawned {
    fn wait(&mut self) -> portable_pty::ExitStatus {
        self.child.wait().unwrap()
    }
}
#[test]
fn a_failed_trampoline_materialize_is_not_cached() {
    use std::io::{Error, ErrorKind};
    let slot = OnceLock::new();
    let missing = remember_path(
        &slot,
        Err(Error::new(ErrorKind::PermissionDenied, "noexec")),
    );
    assert!(missing.is_err());
    let ready = remember_path(&slot, Ok("/tmp/phux-fd-shrink".into()));
    assert_eq!(ready.unwrap().as_os_str(), "/tmp/phux-fd-shrink");
}

#[test]
fn a_missing_trampoline_falls_back_only_for_non_strict_spawns() {
    assert_eq!(spawn_route(false, false), SpawnRoute::Fallback);
    assert_eq!(spawn_route(false, true), SpawnRoute::FailClosed);
    assert_eq!(spawn_route(true, false), SpawnRoute::Launcher);
    assert_eq!(spawn_route(true, true), SpawnRoute::Launcher);
}

fn spawn(cmd: &CommandBuilder) -> Result<Spawned, LaunchDiagnostic> {
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize::default())
        .unwrap();
    let child = spawn_pane_child(&*pair.slave, &*pair.master, cmd)?;
    Ok(Spawned {
        child,
        _master: pair.master,
    })
}

#[test]
fn relative_executable_uses_effective_directory() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("only-here");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut cmd = CommandBuilder::new("./only-here");
    super::super::spawn::apply_strict_spawn_cwd(&mut cmd, dir.path());
    assert!(!spawn_envp(&cmd).unwrap().iter().any(|entry| {
        entry
            .as_bytes()
            .starts_with(super::super::spawn::STRICT_CWD_KEY.as_bytes())
    }));
    assert_eq!(spawn(&cmd).unwrap().wait().exit_code(), 0);
}

#[test]
fn requested_exec_failures_are_not_exit_127() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad-interpreter");
    std::fs::write(&bad, "#!/no/such/interpreter\n").unwrap();
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o700)).unwrap();
    let denied = dir.path().join("denied");
    std::fs::write(&denied, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o600)).unwrap();
    for (program, errno) in [
        (dir.path().join("missing"), libc::ENOENT),
        (bad, libc::ENOENT),
        (denied, libc::EACCES),
        (dir.path().to_path_buf(), libc::EACCES),
    ] {
        let error = spawn(&CommandBuilder::new(program))
            .err()
            .expect("exec must fail");
        assert_eq!(
            error,
            LaunchDiagnostic {
                stage: LaunchStage::Exec,
                errno: Some(errno)
            }
        );
    }
}

#[test]
fn real_zero_nonzero_and_127_are_successful_execs() {
    for code in [0, 7, 127] {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", &format!("exit {code}")]);
        assert_eq!(spawn(&cmd).unwrap().wait().exit_code(), code);
    }
}

#[test]
fn vanished_directory_reports_cwd_without_running_in_home() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("removed");
    let marker = dir.path().join("must-not-run");
    let mut cmd = CommandBuilder::new("/usr/bin/touch");
    cmd.arg(&marker);
    super::super::spawn::apply_strict_spawn_cwd(&mut cmd, &missing);
    let error = spawn(&cmd).err().expect("cwd must fail");
    assert_eq!(
        error,
        LaunchDiagnostic {
            stage: LaunchStage::Cwd,
            errno: Some(libc::ENOENT)
        }
    );
    assert!(!marker.exists());
}
