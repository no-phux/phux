//! The kqueue/epoll reactor behind the park.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

/// The poller quiet PTY masters register with.
#[cfg(target_os = "linux")]
pub(in crate::actor) struct Reactor(nix::sys::epoll::Epoll);

#[cfg(target_os = "linux")]
impl Reactor {
    pub(in crate::actor) fn start() -> io::Result<Self> {
        let flags = nix::sys::epoll::EpollCreateFlags::EPOLL_CLOEXEC;
        Ok(Self(nix::sys::epoll::Epoll::new(flags)?))
    }

    pub(in crate::actor) fn add(&self, fd: std::os::fd::BorrowedFd<'_>, id: u64) -> io::Result<()> {
        let event = nix::sys::epoll::EpollEvent::new(nix::sys::epoll::EpollFlags::EPOLLIN, id);
        Ok(self.0.add(fd, event)?)
    }

    pub(in crate::actor) fn delete(&self, fd: std::os::fd::BorrowedFd<'_>) {
        let _ = self.0.delete(fd);
    }

    pub(in crate::actor) fn wait(&self) -> io::Result<Vec<u64>> {
        let mut events = [nix::sys::epoll::EpollEvent::empty(); 64];
        let n = self
            .0
            .wait(&mut events, nix::sys::epoll::EpollTimeout::NONE)?;
        Ok(events
            .iter()
            .take(n)
            .map(nix::sys::epoll::EpollEvent::data)
            .collect())
    }
}

/// The poller quiet PTY masters register with (kqueue: macOS and the BSDs).
#[cfg(not(target_os = "linux"))]
pub(in crate::actor) struct Reactor(nix::sys::event::Kqueue);

#[cfg(not(target_os = "linux"))]
impl Reactor {
    pub(in crate::actor) fn start() -> io::Result<Self> {
        Ok(Self(nix::sys::event::Kqueue::new()?))
    }

    pub(in crate::actor) fn add(&self, fd: std::os::fd::BorrowedFd<'_>, id: u64) -> io::Result<()> {
        let change = kqueue_change(fd, nix::sys::event::EvFlags::EV_ADD, id)?;
        self.0
            .kevent(std::slice::from_ref(&change), &mut [], None)?;
        Ok(())
    }

    pub(in crate::actor) fn delete(&self, fd: std::os::fd::BorrowedFd<'_>) {
        if let Ok(change) = kqueue_change(fd, nix::sys::event::EvFlags::EV_DELETE, 0) {
            let _ = self.0.kevent(std::slice::from_ref(&change), &mut [], None);
        }
    }

    pub(in crate::actor) fn wait(&self) -> io::Result<Vec<u64>> {
        let empty = kevent(0, nix::sys::event::EvFlags::empty(), 0);
        let mut events = vec![empty; 64];
        let n = self.0.kevent(&[], &mut events, None)?;
        Ok(events
            .iter()
            .take(n)
            .filter(|event| !event.flags().contains(nix::sys::event::EvFlags::EV_ERROR))
            .filter_map(|event| u64::try_from(event.udata()).ok())
            .collect())
    }
}

#[cfg(not(target_os = "linux"))]
fn kevent(ident: usize, flags: nix::sys::event::EvFlags, udata: isize) -> nix::sys::event::KEvent {
    nix::sys::event::KEvent::new(
        ident,
        nix::sys::event::EventFilter::EVFILT_READ,
        flags,
        nix::sys::event::FilterFlag::empty(),
        0,
        udata,
    )
}

#[cfg(not(target_os = "linux"))]
fn kqueue_change(
    fd: std::os::fd::BorrowedFd<'_>,
    flags: nix::sys::event::EvFlags,
    id: u64,
) -> io::Result<nix::sys::event::KEvent> {
    use std::os::fd::AsRawFd;
    let ident = usize::try_from(fd.as_raw_fd())
        .map_err(|_| io::Error::other("pty fd does not fit kevent ident"))?;
    let udata =
        isize::try_from(id).map_err(|_| io::Error::other("pty id does not fit kevent udata"))?;
    Ok(kevent(ident, flags, udata))
}

/// Spawn the poller thread once the park is stored in the process-lifetime slot.
pub(in crate::actor) fn boot_poller(park: &'static Park) {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| park.boot());
}
