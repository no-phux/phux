//! Wake on "an interface address was added": a netlink route socket on
//! Linux, a `PF_ROUTE` socket on macOS. Best effort: where neither opens,
//! [`AddrEvents::changed`] never fires and the caller's backoff alone paces
//! it.

use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

/// A subscription to interface-address changes.
pub(super) struct AddrEvents {
    fd: Option<AsyncFd<OwnedFd>>,
}

impl AddrEvents {
    /// Subscribe, or hold nothing when this platform or process cannot.
    pub(super) fn subscribe() -> Self {
        let fd = open()
            // SAFETY: `fd` is an open descriptor this process owns; `AsyncFd`
            // keeps it open and returns the same raw fd until it is dropped.
            .and_then(|fd| unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE) }.map_err(io::Error::from))
            .inspect_err(|err| {
                tracing::debug!(error = %err, "no address-change events; polling only");
            })
            .ok();
        Self { fd }
    }

    /// Whether this holds a live subscription.
    #[cfg(test)]
    pub(super) const fn is_live(&self) -> bool {
        self.fd.is_some()
    }

    /// Resolve once an address was added since the last call, draining
    /// whatever else is queued so a burst wakes the caller once. A read
    /// error drops the subscription (pending forever after), so a broken
    /// socket cannot spin the caller.
    pub(super) async fn changed(&mut self) {
        let mut buf = [0_u8; 8192];
        let mut added = false;
        loop {
            let Some(fd) = &self.fd else {
                return std::future::pending().await;
            };
            let Ok(mut guard) = fd.readable().await else {
                self.fd = None;
                continue;
            };
            match guard.try_io(|fd| recv(fd.get_ref(), &mut buf)) {
                Ok(Ok(len)) => added |= is_address_added(&buf[..len]),
                Ok(Err(_)) => {
                    self.fd = None;
                    return;
                }
                // Drained: readiness is cleared until the next message.
                Err(_would_block) if added => return,
                Err(_would_block) => {}
            }
        }
    }
}

fn recv(fd: &OwnedFd, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `buf` is valid for writes of `buf.len()` bytes for the call.
    let len = unsafe { libc::recv(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
    usize::try_from(len).map_err(|_| io::Error::last_os_error())
}

fn owned(fd: libc::c_int) -> io::Result<OwnedFd> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor this process owns and nothing
    // else holds.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn open() -> io::Result<OwnedFd> {
    // SAFETY: socket(2) with constant arguments; the result is checked.
    let fd = owned(unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_ROUTE,
        )
    })?;
    // SAFETY: an all-zero `sockaddr_nl` is a valid value.
    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::sa_family_t::try_from(libc::AF_NETLINK).unwrap_or_default();
    addr.nl_groups =
        u32::try_from(libc::RTMGRP_IPV4_IFADDR | libc::RTMGRP_IPV6_IFADDR).unwrap_or_default();
    let len =
        libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_nl>()).unwrap_or_default();
    // SAFETY: `addr` is a valid `sockaddr_nl` of `len` bytes for the call.
    if unsafe { libc::bind(fd.as_raw_fd(), (&raw const addr).cast(), len) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// The group subscription already filters to address messages.
#[cfg(target_os = "linux")]
fn is_address_added(msg: &[u8]) -> bool {
    // `nlmsghdr`: u32 length, then u16 type.
    msg.get(4..6)
        .is_some_and(|ty| u16::from_ne_bytes([ty[0], ty[1]]) == libc::RTM_NEWADDR)
}

#[cfg(target_os = "macos")]
fn open() -> io::Result<OwnedFd> {
    // SAFETY: socket(2) with constant arguments; the result is checked.
    let fd = owned(unsafe { libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, libc::AF_UNSPEC) })?;
    let raw = fd.as_raw_fd();
    // SAFETY: fcntl(2) on a descriptor this function owns.
    let set = unsafe {
        libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) == 0
            && libc::fcntl(raw, libc::F_SETFL, libc::O_NONBLOCK) == 0
    };
    if set {
        Ok(fd)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A routing socket sees every route change; only `RTM_NEWADDR` counts.
#[cfg(target_os = "macos")]
fn is_address_added(msg: &[u8]) -> bool {
    // `rt_msghdr` / `ifa_msghdr`: u16 length, u8 version, u8 type.
    msg.get(3)
        .is_some_and(|&ty| libc::c_int::from(ty) == libc::RTM_NEWADDR)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open() -> io::Result<OwnedFd> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const fn is_address_added(_: &[u8]) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn subscribing_opens_a_route_socket_here() {
        assert!(AddrEvents::subscribe().is_live());
    }

    #[test]
    fn short_messages_are_not_address_events() {
        assert!(!is_address_added(&[]));
        assert!(!is_address_added(&[0, 0, 0]));
    }
}
