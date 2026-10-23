//! Spawn a pane command without inheriting the server's fd table.
//!
//! The child image is [`fd_shrink.c`]: on Linux it replaces the fat
//! post-fork fd table with one sized to the fds the shell actually holds,
//! then `exec`s the requested program. macOS uses `posix_spawn` with
//! `POSIX_SPAWN_CLOEXEC_DEFAULT`, so the kernel builds that small table
//! directly. Either way the server remains the parent of the final shell.

use std::ffi::{CString, OsStr};
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use phux_core::{LaunchDiagnostic, LaunchStage};
use portable_pty::{Child, CommandBuilder, MasterPty, SlavePty};

mod exec_status;

#[cfg(phux_fd_shrink)]
const TRAMPOLINE_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/phux-fd-shrink"));

const SPAWN_CWD_KEY: &str = "PHUX_FD_SHRINK_CWD";

#[cfg(test)]
mod tests;

/// Spawn exactly once and wait for the requested exec, not just the launcher.
/// Fail closed if the verified launcher is unavailable: a fallback must not
/// execute a command without equivalent execution evidence.
pub(super) fn spawn_pane_child(
    _slave: &(dyn SlavePty + Send),
    master: &dyn MasterPty,
    cmd: &CommandBuilder,
) -> Result<Box<dyn Child + Send + Sync>, LaunchDiagnostic> {
    posix_spawn_child(master, cmd)
}

fn trampoline_path() -> io::Result<PathBuf> {
    #[cfg(phux_fd_shrink)]
    {
        static PATH: OnceLock<io::Result<PathBuf>> = OnceLock::new();
        match PATH.get_or_init(materialize_trampoline) {
            Ok(path) => Ok(path.clone()),
            Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
        }
    }
    #[cfg(not(phux_fd_shrink))]
    {
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "fd-table trampoline was not compiled",
        ))
    }
}

#[cfg(phux_fd_shrink)]
fn materialize_trampoline() -> io::Result<PathBuf> {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut hash = String::with_capacity(64);
    for byte in sha2::Sha256::digest(TRAMPOLINE_BYTES) {
        let _ = write!(hash, "{byte:02x}");
    }
    let path = dir.join(format!("phux-fd-shrink-{hash}"));
    let tmp = dir.join(format!("phux-fd-shrink-{hash}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, TRAMPOLINE_BYTES)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn cache_dir() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("phux");
    }
    std::env::temp_dir().join(format!("phux-fd-shrink-{}", nix::unistd::Uid::current()))
}

fn posix_spawn_child(
    master: &dyn MasterPty,
    cmd: &CommandBuilder,
) -> Result<Box<dyn Child + Send + Sync>, LaunchDiagnostic> {
    let (pid, reader) = spawn_launcher(master, cmd).map_err(|error| LaunchDiagnostic {
        stage: LaunchStage::Spawn,
        errno: error.raw_os_error(),
    })?;
    exec_status::confirm(pid, reader)?;
    Ok(Box::new(portable_pty_adopt::AdoptedChild::new(pid)))
}

/// Complete all fallible setup before ownership of this child leaves the
/// launcher seam. After spawn, only the bounded status reader may fail.
fn spawn_launcher(
    master: &dyn MasterPty,
    cmd: &CommandBuilder,
) -> io::Result<(libc::pid_t, OwnedFd)> {
    let trampoline = trampoline_path()?;
    let slave_name = master
        .tty_name()
        .ok_or_else(|| io::Error::new(ErrorKind::Unsupported, "pty master has no slave path"))?;
    let slave = open_slave(&slave_name)?;
    let argv = spawn_argv(&trampoline, cmd)?;
    let envp = spawn_envp(cmd)?;
    let (reader, writer) = exec_status::channel()?;
    let pid = spawn_with_stdio(
        slave.as_raw_fd(),
        &trampoline,
        &argv,
        &envp,
        writer.as_raw_fd(),
    )?;
    drop(writer);
    Ok((pid, reader))
}

fn open_slave(path: &Path) -> io::Result<OwnedFd> {
    let fd = nix::fcntl::open(
        path,
        nix::fcntl::OFlag::O_RDWR | nix::fcntl::OFlag::O_NOCTTY | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(io::Error::from)?;
    Ok(fd)
}

fn spawn_argv(trampoline: &Path, cmd: &CommandBuilder) -> io::Result<Vec<CString>> {
    let mut argv = Vec::with_capacity(cmd.get_argv().len() + 2);
    argv.push(c_os(trampoline.as_os_str())?);
    argv.push(CString::new("--").map_err(|err| io::Error::new(ErrorKind::InvalidInput, err))?);
    for arg in cmd.get_argv() {
        argv.push(c_os(arg)?);
    }
    Ok(argv)
}

fn spawn_envp(cmd: &CommandBuilder) -> io::Result<Vec<CString>> {
    let mut envp = Vec::new();
    for (key, value) in cmd.iter_full_env_as_str() {
        if key == SPAWN_CWD_KEY || key == super::spawn::STRICT_CWD_KEY {
            continue;
        }
        envp.push(c_pair(key, value)?);
    }
    if let Some(dir) = spawn_directory(cmd) {
        envp.push(c_pair(SPAWN_CWD_KEY, &dir.to_string_lossy())?);
    }
    Ok(envp)
}

/// Directory portable-pty would have applied: explicit cwd, else `$HOME`,
/// else the passwd home. `None` inherits the server cwd.
fn spawn_directory(cmd: &CommandBuilder) -> Option<PathBuf> {
    if let Some(cwd) = cmd.get_cwd() {
        let path = PathBuf::from(cwd);
        if cmd.get_env(super::spawn::STRICT_CWD_KEY).is_some() || path.is_dir() {
            return Some(path);
        }
    }
    if let Some(home) = cmd.get_env("HOME") {
        let path = PathBuf::from(home);
        if path.is_dir() {
            return Some(path);
        }
    }
    nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .ok()
        .flatten()
        .map(|user| user.dir)
}

fn c_os(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|err| io::Error::new(ErrorKind::InvalidInput, err))
}

fn c_pair(key: &str, value: &str) -> io::Result<CString> {
    CString::new(format!("{key}={value}"))
        .map_err(|err| io::Error::new(ErrorKind::InvalidInput, err))
}

fn spawn_with_stdio(
    slave: std::os::fd::RawFd,
    trampoline: &Path,
    argv: &[CString],
    envp: &[CString],
    status_fd: std::os::fd::RawFd,
) -> io::Result<libc::pid_t> {
    let mut actions = SpawnActions::new()?;
    for target in 0..3 {
        actions.dup2(slave, target)?;
    }
    // fd 3 belongs only to the launcher. Its own exec closes it, never the
    // server's upgrade fd or the requested program's inherited descriptor.
    actions.dup2(status_fd, 3)?;
    #[cfg(target_os = "macos")]
    let attr = CloexecAttr::new()?;
    #[cfg(not(target_os = "macos"))]
    let attr = ();

    let mut argv_ptrs: Vec<*mut libc::c_char> =
        argv.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
    argv_ptrs.push(std::ptr::null_mut());
    let mut env_ptrs: Vec<*mut libc::c_char> =
        envp.iter().map(|entry| entry.as_ptr().cast_mut()).collect();
    env_ptrs.push(std::ptr::null_mut());

    let path = c_os(trampoline.as_os_str())?;
    let mut pid: libc::pid_t = 0;
    // SAFETY: `actions` and `attr` are initialized for this call and
    // destroyed by their drops. `argv_ptrs` / `env_ptrs` are null-terminated
    // and point at the CStrings that outlive `posix_spawn`. The slave fd
    // stays open until this function returns.
    let rc = unsafe {
        libc::posix_spawn(
            &raw mut pid,
            path.as_ptr(),
            actions.as_ptr(),
            attr.ptr(),
            argv_ptrs.as_ptr(),
            env_ptrs.as_ptr(),
        )
    };
    if rc == 0 {
        Ok(pid)
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}

trait AttrPtr {
    fn ptr(&self) -> *const libc::posix_spawnattr_t;
}

impl AttrPtr for () {
    fn ptr(&self) -> *const libc::posix_spawnattr_t {
        std::ptr::null()
    }
}

struct SpawnActions(libc::posix_spawn_file_actions_t);

impl SpawnActions {
    fn new() -> io::Result<Self> {
        let mut actions = std::mem::MaybeUninit::uninit();
        // SAFETY: `posix_spawn_file_actions_init` writes a fresh actions
        // object through this out-pointer and does not retain it.
        let rc = unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        // SAFETY: init succeeded, so the object is initialized.
        Ok(Self(unsafe { actions.assume_init() }))
    }

    fn dup2(&mut self, from: std::os::fd::RawFd, to: i32) -> io::Result<()> {
        // SAFETY: `self.0` was initialized and is destroyed in `Drop`.
        let rc = unsafe { libc::posix_spawn_file_actions_adddup2(&raw mut self.0, from, to) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(rc))
        }
    }

    const fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
        &raw const self.0
    }
}

impl Drop for SpawnActions {
    fn drop(&mut self) {
        // SAFETY: `self.0` was initialized exactly once and is not used after
        // this destroy.
        unsafe { libc::posix_spawn_file_actions_destroy(&raw mut self.0) };
    }
}

#[cfg(target_os = "macos")]
struct CloexecAttr(libc::posix_spawnattr_t);

#[cfg(target_os = "macos")]
impl CloexecAttr {
    fn new() -> io::Result<Self> {
        let mut attr = std::mem::MaybeUninit::uninit();
        // SAFETY: init writes a fresh attribute object through this pointer.
        let rc = unsafe { libc::posix_spawnattr_init(attr.as_mut_ptr()) };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        // SAFETY: init succeeded.
        let mut attr = Self(unsafe { attr.assume_init() });
        // Apple's flag is an `int`. The setter takes `c_short`; 0x4000 fits.
        let Ok(flags) = libc::c_short::try_from(libc::POSIX_SPAWN_CLOEXEC_DEFAULT) else {
            return Err(io::Error::other(
                "POSIX_SPAWN_CLOEXEC_DEFAULT does not fit posix_spawnattr_setflags",
            ));
        };
        // SAFETY: `attr.0` is initialized and destroyed in `Drop`.
        let rc = unsafe { libc::posix_spawnattr_setflags(&raw mut attr.0, flags) };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        Ok(attr)
    }
}

#[cfg(target_os = "macos")]
impl AttrPtr for CloexecAttr {
    fn ptr(&self) -> *const libc::posix_spawnattr_t {
        &raw const self.0
    }
}

#[cfg(target_os = "macos")]
impl Drop for CloexecAttr {
    fn drop(&mut self) {
        // SAFETY: `self.0` was initialized exactly once.
        unsafe { libc::posix_spawnattr_destroy(&raw mut self.0) };
    }
}
