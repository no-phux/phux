//! The `sockaddr_un` path limit, checked alike by server bind and client
//! connect.

use std::path::Path;

/// Maximum byte length of a Unix-domain-socket path: `sun_path` (108 bytes on
/// Linux, 104 elsewhere) minus the trailing NUL.
pub const MAX_SOCKET_PATH_LEN: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

/// Why `path` can never be bound or dialed, when it is too long for a
/// `sockaddr_un`. Server bind and client connect both say this, rather
/// than the kernel's opaque "path must be shorter than `SUN_LEN`".
#[must_use]
pub fn path_too_long(path: &Path) -> Option<String> {
    let len = path.as_os_str().len();
    (len > MAX_SOCKET_PATH_LEN).then(|| path_too_long_message(path, len))
}

/// The sentence for a socket path of `len` bytes over [`MAX_SOCKET_PATH_LEN`].
#[must_use]
pub fn path_too_long_message(path: &Path, len: usize) -> String {
    format!(
        "socket path {} is {len} bytes, but unix domain socket paths on this platform are \
         limited to {MAX_SOCKET_PATH_LEN} bytes; pick a shorter path (under /tmp, for \
         example) with PHUX_SOCKET or --socket",
        path.display()
    )
}
