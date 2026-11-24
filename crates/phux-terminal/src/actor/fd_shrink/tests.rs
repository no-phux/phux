//! Real PTY requested-exec classification, not launcher exit-code inference.
//! Also the trampoline cache and non-strict fallback decision.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use phux_terminal_api::launch::LaunchCommand;
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
    let missing = remember_path(&slot, || {
        Err(Error::new(ErrorKind::PermissionDenied, "noexec"))
    });
    assert!(missing.is_err());
    let ready = remember_path(&slot, || Ok("/tmp/phux-fd-shrink".into()));
    assert_eq!(ready.unwrap().as_os_str(), "/tmp/phux-fd-shrink");
}

#[test]
fn a_deleted_cached_trampoline_is_materialized_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("launcher");
    let slot = OnceLock::new();
    let materialize = || {
        std::fs::write(&path, b"launcher")?;
        Ok(path.clone())
    };
    assert_eq!(remember_path(&slot, materialize).unwrap(), path);
    assert_eq!(remember_path(&slot, || panic!("cache miss")).unwrap(), path);
    std::fs::remove_file(&path).unwrap();
    let failed = remember_path(&slot, || {
        Err(io::Error::new(ErrorKind::PermissionDenied, "cannot repair"))
    });
    assert!(failed.is_err());
    assert_eq!(remember_path(&slot, materialize).unwrap(), path);
    assert!(path.is_file());
}

#[test]
fn spawn_pane_child_routes_a_missing_trampoline_through_strict_cwd() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            set_force_trampoline_missing(false);
        }
    }
    let _restore = Restore;
    set_force_trampoline_missing(true);
    // `/bin/sh`, not `/bin/true`: macOS has only `/usr/bin/true`, so the
    // fallback spawn failed there with ENOENT rather than testing the route.
    let mut strict = CommandBuilder::new("/bin/sh");
    strict.args(["-c", "exit 0"]);
    strict.env(STRICT_CWD_KEY, "1");
    match spawn(&strict) {
        Err(LaunchDiagnostic {
            stage: LaunchStage::Spawn,
            errno: Some(errno),
        }) if errno == libc::ENOEXEC => {}
        Err(error) => panic!("strict missing trampoline returned {error}"),
        Ok(_) => panic!("strict missing trampoline spawned"),
    }
    let mut loose = CommandBuilder::new("/bin/sh");
    loose.args(["-c", "exit 0"]);
    let mut spawned = spawn(&loose).unwrap();
    assert_eq!(spawned.wait().exit_code(), 0);
}

/// `intent` pinned to `cwd` with no fallback, as the server's restore does.
fn strict_cwd_command(mut intent: LaunchCommand, cwd: &Path) -> CommandBuilder {
    phux_terminal_api::launch::apply_strict_spawn_cwd(&mut intent, cwd);
    crate::engine::command(&intent)
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
    let cmd = strict_cwd_command(LaunchCommand::new("./only-here"), dir.path());
    assert!(
        !spawn_envp(&cmd)
            .unwrap()
            .iter()
            .any(|entry| { entry.as_bytes().starts_with(STRICT_CWD_KEY.as_bytes()) })
    );
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
fn executable_text_requires_a_shebang_or_an_explicit_shell() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("plain-text");
    let later = dir.path().join("later");
    std::fs::create_dir(&later).unwrap();
    std::fs::write(later.join("plain-text"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        later.join("plain-text"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let marker = dir.path().join("ran");
    std::fs::write(&program, "printf '%s' \"$2\" > \"$1\"\nexit 127\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut direct = CommandBuilder::new(&program);
    direct.args([marker.as_os_str(), OsStr::new("implicit")]);
    let mut searched = CommandBuilder::new("plain-text");
    searched.cwd(dir.path());
    searched.env("PATH", ".:later");
    searched.args([marker.as_os_str(), OsStr::new("implicit")]);
    for command in [&direct, &searched] {
        let error = match spawn(command) {
            Err(error) => error,
            Ok(mut child) => panic!("implicitly ran shell, status {}", child.wait().exit_code()),
        };
        assert_eq!(error.stage, LaunchStage::Exec);
        assert_eq!(error.errno, Some(libc::ENOEXEC));
        assert!(!marker.exists());
    }

    let mut explicit = CommandBuilder::new("/bin/sh");
    explicit.args([
        program.as_os_str(),
        marker.as_os_str(),
        OsStr::new("explicit"),
    ]);
    assert_eq!(spawn(&explicit).unwrap().wait().exit_code(), 127);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "explicit");
}

#[test]
fn path_lookup_preserves_cwd_search_order_and_literal_shebang_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let denied = dir.path().join("denied");
    let later = dir.path().join("later");
    std::fs::create_dir(&denied).unwrap();
    std::fs::create_dir(&later).unwrap();
    std::fs::write(denied.join("probe"), "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(denied.join("probe"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(later.join("probe"), "#!/bin/sh\nexit 2\n").unwrap();
    std::fs::write(
        dir.path().join("probe"),
        "#!/bin/sh\nprintf '<%s>\\n' \"$2\" \"$3\" > \"$1\"\nexit 127\n",
    )
    .unwrap();
    for script in [dir.path().join("probe"), later.join("probe")] {
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let marker = dir.path().join("argv");
    let mut cmd = strict_cwd_command(LaunchCommand::new("probe"), dir.path());
    cmd.args([marker.as_os_str(), OsStr::new("a b; $HOME"), OsStr::new("")]);
    cmd.env("PATH", "missing:denied::later");
    assert_eq!(spawn(&cmd).unwrap().wait().exit_code(), 127);
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        "<a b; $HOME>\n<>\n"
    );
    cmd.env("PATH", "missing:denied");
    assert_eq!(spawn(&cmd).err().unwrap().errno, Some(libc::EACCES));
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
    let mut intent = LaunchCommand::new("/usr/bin/touch");
    intent.arg(&marker);
    let cmd = strict_cwd_command(intent, &missing);
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
