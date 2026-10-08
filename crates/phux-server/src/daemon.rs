//! Descriptor hygiene for a daemon spawned by a short-lived client.
//!
//! An auto-spawned server outlives the client that started it, so every
//! descriptor it inherits stays open for the server's whole lifetime. Rust
//! opens its own descriptors close-on-exec, so what a spawner would pass along
//! is what *it* inherited from its caller. On macOS that includes a sibling's
//! capture pipe: `Command` there creates pipes with `pipe()` and sets
//! `FD_CLOEXEC` in a second step, so a caller spawning from several threads
//! can leak one child's stdout pipe into another. Handed on to the daemon, that
//! pipe never reaches EOF, and the caller's read of a `phux server --ensure`
//! that exited long ago blocks until the server does (phux-5wxp.5).

use std::os::fd::{BorrowedFd, RawFd};

/// Mark every descriptor above stderr close-on-exec, so the next child this
/// process spawns inherits only the stdio `Command` sets for it.
///
/// For a process about to spawn a daemon and with no descriptor it means any
/// child to inherit. The descriptors stay open here; only future children stop
/// inheriting them. Best-effort: an unreadable descriptor table, or a
/// descriptor closed meanwhile, leaves that flag as it was.
pub fn withhold_inherited_descriptors() {
    let Ok(entries) = std::fs::read_dir(DESCRIPTOR_TABLE) else {
        return;
    };
    // Act while iterating: the listing's own descriptor is still open, and is
    // close-on-exec already.
    for fd in entries.filter_map(|entry| descriptor_number(&entry.ok()?)) {
        if fd > 2 {
            set_cloexec(fd);
        }
    }
}

/// The descriptor a descriptor-table entry names.
fn descriptor_number(entry: &std::fs::DirEntry) -> Option<RawFd> {
    entry.file_name().to_str()?.parse().ok()
}

/// The per-process view of open descriptors.
#[cfg(target_os = "linux")]
const DESCRIPTOR_TABLE: &str = "/proc/self/fd";
#[cfg(not(target_os = "linux"))]
const DESCRIPTOR_TABLE: &str = "/dev/fd";

fn set_cloexec(fd: RawFd) {
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};

    // SAFETY: `fd` was listed in this process's descriptor table a moment ago
    // and the borrow ends within this call; nothing here closes it or takes
    // ownership. If another thread closed it meanwhile, `fcntl` fails with
    // `EBADF`, which is ignored; if the number was reused, the new descriptor
    // was opened close-on-exec by Rust already, so setting the flag is a no-op.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    if let Ok(flags) = fcntl_getfd(borrowed)
        && !flags.contains(FdFlags::CLOEXEC)
    {
        let _ = fcntl_setfd(borrowed, flags | FdFlags::CLOEXEC);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
    use std::os::fd::AsFd;

    fn close_on_exec(fd: impl AsFd) -> bool {
        fcntl_getfd(fd.as_fd())
            .expect("descriptor flags")
            .contains(FdFlags::CLOEXEC)
    }

    #[test]
    fn an_inheritable_descriptor_becomes_close_on_exec() {
        let (reader, writer) = std::io::pipe().expect("pipe");
        let flags = fcntl_getfd(writer.as_fd()).expect("descriptor flags");
        fcntl_setfd(writer.as_fd(), flags.difference(FdFlags::CLOEXEC)).expect("inheritable");
        assert!(!close_on_exec(&writer));

        withhold_inherited_descriptors();

        assert!(close_on_exec(&writer));
        assert!(close_on_exec(&reader));
        // stdio is the caller's to hand on; it is left alone.
        assert!(!close_on_exec(std::io::stderr()));
    }
}
