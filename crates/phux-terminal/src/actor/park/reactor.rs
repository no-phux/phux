//! The kqueue/epoll reactor behind the park.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

pub(in crate::actor) enum Reactor {
    #[cfg(target_os = "linux")]
    Epoll(nix::sys::epoll::Epoll),
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    Kqueue(nix::sys::event::Kqueue),
}

impl Reactor {
    pub(in crate::actor) fn start() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let epoll =
                nix::sys::epoll::Epoll::new(nix::sys::epoll::EpollCreateFlags::EPOLL_CLOEXEC)
                    .map_err(io::Error::from)?;
            return Ok(Self::Epoll(epoll));
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            let kqueue = nix::sys::event::Kqueue::new().map_err(io::Error::from)?;
            return Ok(Self::Kqueue(kqueue));
        }
        #[allow(unreachable_code)]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no pty poller for this platform",
        ))
    }

    pub(in crate::actor) fn add(&self, fd: std::os::fd::BorrowedFd<'_>, id: u64) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let Self::Epoll(epoll) = self;
            let event = nix::sys::epoll::EpollEvent::new(nix::sys::epoll::EpollFlags::EPOLLIN, id);
            return epoll.add(fd, event).map_err(io::Error::from);
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            let Self::Kqueue(kqueue) = self;
            let change = kqueue_change(fd, nix::sys::event::EvFlags::EV_ADD, id)?;
            return kqueue
                .kevent(std::slice::from_ref(&change), &mut [], None)
                .map(|_| ())
                .map_err(io::Error::from);
        }
        #[allow(unreachable_code)]
        {
            let _ = (fd, id);
            Err(io::Error::other("no pty poller"))
        }
    }

    pub(in crate::actor) fn delete(&self, fd: std::os::fd::BorrowedFd<'_>) {
        #[cfg(target_os = "linux")]
        {
            let Self::Epoll(epoll) = self;
            let _ = epoll.delete(fd);
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            let Self::Kqueue(kqueue) = self;
            let Ok(change) = kqueue_change(fd, nix::sys::event::EvFlags::EV_DELETE, 0) else {
                return;
            };
            let _ = kqueue.kevent(std::slice::from_ref(&change), &mut [], None);
        }
    }

    pub(in crate::actor) fn wait(&self) -> io::Result<Vec<u64>> {
        #[cfg(target_os = "linux")]
        {
            let Self::Epoll(epoll) = self;
            let mut events = [nix::sys::epoll::EpollEvent::empty(); 64];
            let n = epoll
                .wait(&mut events, nix::sys::epoll::EpollTimeout::NONE)
                .map_err(io::Error::from)?;
            return Ok(events
                .iter()
                .take(n)
                .map(nix::sys::epoll::EpollEvent::data)
                .collect());
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            let Self::Kqueue(kqueue) = self;
            let mut events = vec![
                nix::sys::event::KEvent::new(
                    0,
                    nix::sys::event::EventFilter::EVFILT_READ,
                    nix::sys::event::EvFlags::empty(),
                    nix::sys::event::FilterFlag::empty(),
                    0,
                    0,
                );
                64
            ];
            let n = kqueue
                .kevent(&[], &mut events, None)
                .map_err(io::Error::from)?;
            return Ok(events
                .iter()
                .take(n)
                .filter(|event| !event.flags().contains(nix::sys::event::EvFlags::EV_ERROR))
                .filter_map(|event| u64::try_from(event.udata()).ok())
                .collect());
        }
        #[allow(unreachable_code)]
        Err(io::Error::other("no pty poller"))
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
pub(in crate::actor) fn kqueue_change(
    fd: std::os::fd::BorrowedFd<'_>,
    flags: nix::sys::event::EvFlags,
    id: u64,
) -> io::Result<nix::sys::event::KEvent> {
    use std::os::fd::AsRawFd;
    let ident = usize::try_from(fd.as_raw_fd())
        .map_err(|_| io::Error::other("pty fd does not fit kevent ident"))?;
    let udata =
        isize::try_from(id).map_err(|_| io::Error::other("pty id does not fit kevent udata"))?;
    Ok(nix::sys::event::KEvent::new(
        ident,
        nix::sys::event::EventFilter::EVFILT_READ,
        flags,
        nix::sys::event::FilterFlag::empty(),
        0,
        udata,
    ))
}

/// Spawn the poller thread once the park is stored in the process-lifetime slot.
pub(in crate::actor) fn boot_poller(park: &'static Park) {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| park.boot());
}
