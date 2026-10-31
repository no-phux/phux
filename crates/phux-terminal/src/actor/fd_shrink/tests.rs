//! Strict cwd reaches both spawn paths; relative argv uses that cwd.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn relative_executable_is_checked_in_effective_execution_directory() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("only-here");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut cmd = CommandBuilder::new("./only-here");
    super::super::spawn::apply_strict_spawn_cwd(&mut cmd, dir.path());
    assert!(program_is_executable(&cmd, OsStr::new("./only-here")));
    assert!(!spawn_envp(&cmd).unwrap().iter().any(|entry| {
        entry
            .as_bytes()
            .starts_with(super::super::spawn::STRICT_CWD_KEY.as_bytes())
    }));
}
#[test]
fn vanished_explicit_directory_never_executes_in_home_or_portable_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("removed");
    let marker = dir.path().join("must-not-run");
    let mut cmd = CommandBuilder::new("/usr/bin/touch");
    cmd.arg(&marker);
    super::super::spawn::apply_strict_spawn_cwd(&mut cmd, &missing);
    assert_eq!(spawn_directory(&cmd), Some(missing));
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize::default())
        .unwrap();
    assert!(fallback_spawn(&*pair.slave, &mut cmd).is_err());
    let result = spawn_pane_child(&*pair.slave, &*pair.master, cmd);
    if let Ok(mut child) = result {
        let _ = child.kill();
        let _ = child.wait();
        panic!("strict cwd unexpectedly spawned");
    }
    assert!(!marker.exists());
}
