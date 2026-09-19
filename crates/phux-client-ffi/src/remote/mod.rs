//! The C surface over the runtime's relay tunnel (ADR-0133): how a native
//! embedder reaches a registered remote phux server the way
//! `phux attach --remote HOST` does (ADR-0007, ADR-0031, ADR-0093 rung 1).
//!
//! Resolution, dial planning, trust rules, the relay itself, and the
//! tunnel's lifecycle all live in `phux_client_runtime`; this module owns
//! only the opaque handle, the `#[repr(C)]` structs, and the exported
//! functions. The bearer token is read inside the runtime's tunnel thread
//! and never crosses the C ABI.

#![allow(
    clippy::redundant_pub_crate,
    reason = "the private remote module's crate-visible items serve the crate-root C exports"
)]

mod captured;
pub mod registry;

pub use captured::phux_remote_tunnel_clone_resolved;

use std::ffi::c_int;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::{mem, ptr};

use phux_client_runtime::target::{self, Transport};
use phux_client_runtime::tunnel::{Tunnel, TunnelStartError, TunnelState};

use crate::error::{BridgeError, bytes_in, check_struct};
use crate::types::{PhuxBytes, PhuxClientResult, bytes_out};

/// Registry entry found; no network activity yet.
pub const REMOTE_TUNNEL_RESOLVED: u32 = TunnelState::Resolved.as_u32();
/// `phux_remote_tunnel_start` accepted the transport socket; dialing.
pub const REMOTE_TUNNEL_CONNECTING: u32 = TunnelState::Connecting.as_u32();
/// Transport established; frames are being relayed.
pub const REMOTE_TUNNEL_CONNECTED: u32 = TunnelState::Connected.as_u32();
/// Resolution or the connection failed; `message` says why. Terminal.
pub const REMOTE_TUNNEL_FAILED: u32 = TunnelState::Failed.as_u32();
/// The embedder closed its end, or the tunnel was freed. Terminal.
pub const REMOTE_TUNNEL_CLOSED: u32 = TunnelState::Closed.as_u32();

/// No transport: resolution failed.
pub const REMOTE_TRANSPORT_NONE: u32 = 0;
/// `quic://HOST:PORT` (ADR-0007).
pub const REMOTE_TRANSPORT_QUIC: u32 = 1;
/// `wss://` or loopback `ws://`.
pub const REMOTE_TRANSPORT_WS: u32 = 2;

/// Bounds on the caller's spans: a target is a host label, not a document.
const MAX_TARGET_BYTES: usize = 1024;
pub(crate) const MAX_CONFIG_PATH_BYTES: usize = 4096;

/// What to resolve. Initialize `size`/`version` like every ABI struct.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxRemoteTarget {
    pub size: usize,
    pub version: u32,
    /// Registry name or `[USER@]HOST[:PORT]`, UTF-8 without NUL.
    pub target: PhuxBytes,
    /// Absolute path of the phux `config.toml` to read, or empty for the
    /// CLI's own resolution (`$XDG_CONFIG_HOME/phux/config.toml`, else
    /// `~/.config/phux/config.toml`).
    pub config_path: PhuxBytes,
}

/// A tunnel's displayable state. Spans are borrowed from the tunnel and stay
/// valid until `phux_remote_tunnel_free`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxRemoteTunnelInfo {
    pub size: usize,
    pub version: u32,
    pub state: u32,
    pub transport: u32,
    /// The registry entry's name; the typed target when resolution failed.
    pub name: PhuxBytes,
    /// Effective endpoint URI (after any `:PORT` override). Never a token.
    pub endpoint: PhuxBytes,
    /// The entry's pinned `session`, or empty.
    pub session: PhuxBytes,
    /// Why the tunnel failed; empty unless `state` is FAILED.
    pub message: PhuxBytes,
}

/// Opaque tunnel handle.
///
/// One embedder thread at a time owns resolve/start/free. Ownership may move
/// before start, and back after the embedder joins its socket worker; unlike a
/// `PhuxClient`, this handle has no originating-thread affinity.
/// `phux_remote_tunnel_info` may run on any thread while the tunnel lives,
/// including concurrently with `start`: the runtime's tunnel publishes its
/// state through atomics only.
pub struct PhuxRemoteTunnel {
    /// The live tunnel, or the reason resolution failed. A handle for an
    /// unregistered host is FAILED from birth and can never start.
    pub(crate) inner: Result<Tunnel, String>,
    /// Display fields only; no secret.
    pub(crate) name: String,
    pub(crate) endpoint: String,
    pub(crate) session: String,
}

impl std::fmt::Debug for PhuxRemoteTunnel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PhuxRemoteTunnel")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl PhuxRemoteTunnel {
    fn resolve(raw: &str, config_path: Option<&Path>) -> Self {
        match target::resolve(raw, config_path) {
            Ok(resolved) => {
                tracing::info!(
                    host = %resolved.name,
                    endpoint = %resolved.endpoint,
                    transport = resolved.transport.label(),
                    "remote tunnel resolved"
                );
                Self {
                    name: resolved.name.clone(),
                    endpoint: resolved.endpoint.clone(),
                    session: resolved.session.clone().unwrap_or_default(),
                    inner: Ok(Tunnel::new(resolved)),
                }
            }
            Err(message) => {
                tracing::warn!(target = raw.trim(), reason = %message, "remote tunnel resolution failed");
                Self {
                    inner: Err(message),
                    name: raw.trim().to_owned(),
                    endpoint: String::new(),
                    session: String::new(),
                }
            }
        }
    }

    pub(crate) fn tunnel(&self) -> Result<&Tunnel, BridgeError> {
        self.inner
            .as_ref()
            .map_err(|_| BridgeError::state("remote tunnel has no resolved host"))
    }

    fn state(&self) -> u32 {
        self.inner
            .as_ref()
            .map_or(REMOTE_TUNNEL_FAILED, |tunnel| tunnel.state().as_u32())
    }

    fn message(&self) -> &str {
        self.inner
            .as_ref()
            .map_or_else(String::as_str, |tunnel| tunnel.message().unwrap_or(""))
    }

    fn transport(&self) -> u32 {
        self.inner
            .as_ref()
            .map_or(REMOTE_TRANSPORT_NONE, |tunnel| match tunnel.transport() {
                Transport::Quic(_) => REMOTE_TRANSPORT_QUIC,
                Transport::Ws(_) => REMOTE_TRANSPORT_WS,
            })
    }

    fn start(&self, stream: UnixStream) -> Result<(), BridgeError> {
        self.tunnel()?.start(stream).map_err(|err| match err {
            TunnelStartError::NotResolved => BridgeError::state(err.to_string()),
            TunnelStartError::Spawn(_) => BridgeError::engine("could not start the tunnel thread"),
        })
    }
}

pub(crate) fn guard(f: impl FnOnce() -> Result<(), BridgeError>) -> PhuxClientResult {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => PhuxClientResult::Ok,
        Ok(Err(error)) => error.result,
        Err(_) => PhuxClientResult::Panic,
    }
}

/// Decode a bounded UTF-8 span without NUL.
///
/// # Safety
///
/// A non-empty span must be readable for `span.len` bytes for the call.
pub(crate) unsafe fn text_in(
    span: PhuxBytes,
    max: usize,
    field: &str,
) -> Result<&str, BridgeError> {
    if span.len > max {
        return Err(BridgeError::invalid(format!("{field} is too long")));
    }
    // SAFETY: forwarded caller contract; bytes_in rejects a null non-empty span.
    let bytes = unsafe { bytes_in(span.data, span.len) }?;
    let text = std::str::from_utf8(bytes)
        .map_err(|_| BridgeError::invalid(format!("{field} is not UTF-8")))?;
    if text.contains('\0') {
        return Err(BridgeError::invalid(format!("{field} contains NUL")));
    }
    Ok(text)
}

/// Resolve a registered remote host without touching the network.
///
/// Always yields a tunnel on `PHUX_CLIENT_OK`, including for a host that is
/// not registered: that tunnel is FAILED and its `message` names the CLI
/// command that pairs the host. Only malformed arguments fail the call.
///
/// # Safety
///
/// `target` must be readable, its spans readable for their lengths, and
/// `out_tunnel` writable, for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_remote_tunnel_resolve(
    target: *const PhuxRemoteTarget,
    out_tunnel: *mut *mut PhuxRemoteTunnel,
) -> PhuxClientResult {
    guard(|| {
        // SAFETY: checked before write.
        let out = unsafe { out_tunnel.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_tunnel is null"))?;
        *out = ptr::null_mut();
        // SAFETY: checked before dereference.
        let target =
            unsafe { target.as_ref() }.ok_or_else(|| BridgeError::invalid("target is null"))?;
        check_struct(
            target.size,
            mem::size_of::<PhuxRemoteTarget>(),
            target.version,
        )?;
        // SAFETY: the caller's span contract, bounded by text_in.
        let raw = unsafe { text_in(target.target, MAX_TARGET_BYTES, "target") }?;
        // SAFETY: as above.
        let path = unsafe { text_in(target.config_path, MAX_CONFIG_PATH_BYTES, "config_path") }?;
        let config_path = config_path_in(path)?;
        *out = Box::into_raw(Box::new(PhuxRemoteTunnel::resolve(raw, config_path)));
        Ok(())
    })
}

/// Empty selects the CLI's own config path; anything else must be absolute,
/// because a relative path would resolve against whatever the embedder's
/// working directory happens to be.
pub(crate) fn config_path_in(path: &str) -> Result<Option<&Path>, BridgeError> {
    if path.is_empty() {
        return Ok(None);
    }
    if !Path::new(path).is_absolute() {
        return Err(BridgeError::invalid("config_path must be absolute"));
    }
    Ok(Some(Path::new(path)))
}

/// Read a tunnel's state and displayable fields.
///
/// # Safety
///
/// `tunnel` must be live (not freed) and `out_info` writable for the call.
/// Returned spans are valid until `phux_remote_tunnel_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_remote_tunnel_info(
    tunnel: *const PhuxRemoteTunnel,
    out_info: *mut PhuxRemoteTunnelInfo,
) -> PhuxClientResult {
    guard(|| {
        // SAFETY: checked before dereference.
        let tunnel =
            unsafe { tunnel.as_ref() }.ok_or_else(|| BridgeError::invalid("tunnel is null"))?;
        // SAFETY: checked before write.
        let out =
            unsafe { out_info.as_mut() }.ok_or_else(|| BridgeError::invalid("out_info is null"))?;
        check_struct(
            out.size,
            mem::size_of::<PhuxRemoteTunnelInfo>(),
            out.version,
        )?;
        // One load decides both fields: a failure landing between two reads
        // would pair a live state with a reason, and `message` is documented
        // as empty unless `state` is FAILED. FAILED is terminal, so a second
        // read after observing it cannot see anything else.
        let state = tunnel.state();
        out.state = state;
        out.transport = tunnel.transport();
        out.name = bytes_out(tunnel.name.as_bytes());
        out.endpoint = bytes_out(tunnel.endpoint.as_bytes());
        out.session = bytes_out(tunnel.session.as_bytes());
        let message = if state == REMOTE_TUNNEL_FAILED {
            tunnel.message()
        } else {
            ""
        };
        out.message = bytes_out(message.as_bytes());
        Ok(())
    })
}

/// Start dialing, relaying frames through `transport_fd`.
///
/// `transport_fd` is one end of a connected `SOCK_STREAM` Unix-domain socket
/// pair whose other end the embedder keeps for its ordinary framed I/O.
/// Ownership of the descriptor transfers on EVERY return path, including
/// failure. On macOS the embedder should set `SO_NOSIGPIPE` on both ends
/// before the call: the tunnel writes to this descriptor from a library
/// thread and cannot change the host process's `SIGPIPE` disposition.
///
/// When the dial or connection fails the tunnel publishes FAILED and its
/// message, then closes the descriptor, so the embedder reads EOF only after
/// the reason is readable.
///
/// # Safety
///
/// `tunnel` must be live and owned by the calling thread. A non-negative
/// `transport_fd` must be an open descriptor the caller owns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_remote_tunnel_start(
    tunnel: *mut PhuxRemoteTunnel,
    transport_fd: c_int,
) -> PhuxClientResult {
    // SAFETY: the caller transfers an owned open descriptor; adopting it first
    // is what closes it on every error path below.
    let owned = (transport_fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(transport_fd) });
    guard(move || {
        let fd = owned.ok_or_else(|| BridgeError::invalid("transport_fd is negative"))?;
        // SAFETY: checked before dereference.
        // A shared borrow only: `info` may run concurrently on another thread.
        let tunnel =
            unsafe { tunnel.as_ref() }.ok_or_else(|| BridgeError::invalid("tunnel is null"))?;
        tunnel.start(UnixStream::from(fd))
    })
}

/// Stop and free a tunnel: cancel any dial, close the connection, and join
/// the tunnel thread. Bounded by one scheduler poll, never by the network.
///
/// # Safety
///
/// `tunnel` must be null or a live pointer from `phux_remote_tunnel_resolve`,
/// freed once, with no concurrent `phux_remote_tunnel_info`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_remote_tunnel_free(tunnel: *mut PhuxRemoteTunnel) {
    if tunnel.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the caller transfers the unique pointer from resolve once.
        drop(unsafe { Box::from_raw(tunnel) });
    }));
}

#[cfg(test)]
mod tests;
