//! Connected mode: the runtime owns the socket (ADR-0133).
//!
//! `phux_client_new` builds the historical embedded lane, where the
//! embedder dials, reconnects, and pumps frames itself.
//! [`phux_client_connect`] builds the connected lane instead: the runtime's
//! driver resolves the target under the CLI's trust rules, dials, walks the
//! reconnect ladder, and reads and writes the socket on its own thread.
//!
//! The decode point does not move. This ABI's per-frame hooks read
//! workspace-subscription state that only the owning thread may touch, so a
//! connected driver retains inbound frames
//! ([`InboundDelivery::Queued`](phux_client_runtime::control::InboundDelivery::Queued))
//! and [`phux_client_poll`] feeds them from the owning thread, through the
//! same path `phux_client_feed_frame` uses. What an embedder sheds is the
//! dialer, the ladder, and the framing, not the frame semantics.

use std::ffi::c_void;
use std::mem;
use std::path::PathBuf;
use std::sync::Arc;

use phux_client_runtime::{ClientOptions, Listener, Runtime, Target};

use crate::client::{Client, Limits};
use crate::error::{BridgeError, check_struct};
use crate::remote::config_path_in;
use crate::remote::text_in;
use crate::types::{PhuxBytes, PhuxClientOptions, PhuxClientResult};
use crate::{PhuxClient, client_limits, guard, with_client_mut};

/// The longest target a connect call accepts, matching the remote tunnel's.
const MAX_TARGET_BYTES: usize = 1024;
/// The longest config or socket path a connect call accepts.
const MAX_PATH_BYTES: usize = 4096;
/// The longest `HELLO` client name a connect call accepts.
const MAX_CLIENT_NAME_BYTES: usize = 256;

/// Told from the runtime's driver thread that consumer-visible state
/// changed. One outstanding wake covers any number of frames; the next
/// `phux_client_poll` re-arms it.
pub type PhuxClientWakeCallback = unsafe extern "C" fn(context: *mut c_void);

/// Where a connected client dials and what it trusts.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxConnectOptions {
    /// `sizeof(PhuxConnectOptions)`.
    pub size: usize,
    /// `PHUX_CLIENT_ABI_VERSION`.
    pub version: u32,
    /// The payload limits `phux_client_new` takes.
    pub base: PhuxClientOptions,
    /// A registry name or `[USER@]HOST[:PORT]`, resolved exactly as
    /// `phux attach --remote` resolves it. Empty selects `socket_path`.
    pub target: PhuxBytes,
    /// The local server's Unix-domain socket, used when `target` is empty.
    pub socket_path: PhuxBytes,
    /// An absolute `config.toml`, or empty for the CLI's own resolution.
    pub config_path: PhuxBytes,
    /// The `HELLO` client name the runtime sends on every connection.
    pub client_name: PhuxBytes,
    /// Called from the runtime's thread when something changed. May be
    /// NULL, in which case the embedder polls on its own schedule.
    pub wake: Option<PhuxClientWakeCallback>,
    /// Passed back to `wake` untouched.
    pub wake_context: *mut c_void,
}

/// Bridges the runtime's `Listener` onto the embedder's C callback.
struct WakeBridge {
    wake: PhuxClientWakeCallback,
    context: WakeContext,
}

/// The embedder's context pointer. It is only ever handed back to the
/// embedder's own callback, never dereferenced here.
struct WakeContext(*mut c_void);

// SAFETY: the pointer is opaque to this crate and is only passed back to the
// embedder's own callback. The ABI documents that callback as callable from
// the runtime's thread, and the runtime never invokes a listener while
// holding one of its locks.
unsafe impl Send for WakeContext {}
// SAFETY: as above.
unsafe impl Sync for WakeContext {}

impl Listener for WakeBridge {
    fn on_activity(&self) {
        // SAFETY: the embedder's own function pointer and context, which it
        // keeps valid until `phux_client_free` joins the driver thread.
        unsafe { (self.wake)(self.context.0) }
    }
}

/// Start a session whose socket, reconnect ladder and framing belong to the
/// runtime.
///
/// The returned client is otherwise the `phux_client_new` client: the same
/// effects, grid, operations and workspace calls apply, and it keeps the
/// same owning-thread contract. `phux_client_feed_frame` and the
/// `phux_client_outgoing_*` calls are the embedded lane's and refuse here.
///
/// The runtime queues `HELLO` itself on every connection, so a connected
/// embedder must not call `phux_client_queue_hello`. `ATTACH` stays
/// explicit.
///
/// # Safety
///
/// `options` must be readable, its spans readable for their lengths, and
/// `out_client` writable, for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_connect(
    options: *const PhuxConnectOptions,
    out_client: *mut *mut PhuxClient,
) -> PhuxClientResult {
    guard(|| {
        // SAFETY: checked before write.
        let out = unsafe { out_client.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_client is null"))?;
        *out = std::ptr::null_mut();
        // SAFETY: checked before dereference.
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        check_struct(
            options.size,
            mem::size_of::<PhuxConnectOptions>(),
            options.version,
        )?;
        check_struct(
            options.base.size,
            mem::size_of::<PhuxClientOptions>(),
            options.base.version,
        )?;
        let limits = client_limits(&options.base)?;
        // SAFETY: the caller's span contract, bounded by text_in.
        let raw = unsafe { text_in(options.target, MAX_TARGET_BYTES, "target") }?;
        // SAFETY: as above.
        let socket = unsafe { text_in(options.socket_path, MAX_PATH_BYTES, "socket_path") }?;
        // SAFETY: as above.
        let config = unsafe { text_in(options.config_path, MAX_PATH_BYTES, "config_path") }?;
        // SAFETY: as above.
        let name = unsafe { text_in(options.client_name, MAX_CLIENT_NAME_BYTES, "client_name") }?;
        let target = resolve_target(raw, socket, config)?;
        let client = connect(limits, target, name, options.wake, options.wake_context)?;
        *out = Box::into_raw(Box::new(client));
        Ok(())
    })
}

/// A registry target when one is named, otherwise the local socket.
fn resolve_target(raw: &str, socket: &str, config: &str) -> Result<Target, BridgeError> {
    if raw.is_empty() {
        if socket.is_empty() {
            return Err(BridgeError::invalid(
                "connect needs either a target or a socket_path",
            ));
        }
        return Ok(Target::uds(PathBuf::from(socket)));
    }
    if !socket.is_empty() {
        return Err(BridgeError::invalid(
            "connect takes a target or a socket_path, not both",
        ));
    }
    let config_path = config_path_in(config)?;
    phux_client_runtime::target::resolve(raw, config_path)
        .map(Target::from)
        .map_err(BridgeError::invalid)
}

fn connect(
    limits: Limits,
    target: Target,
    client_name: &str,
    wake: Option<PhuxClientWakeCallback>,
    wake_context: *mut c_void,
) -> Result<PhuxClient, BridgeError> {
    let (mut control, history) = Client::control_options(&limits);
    client_name.clone_into(&mut control.client_name);
    let runtime = Runtime::connect(
        target,
        ClientOptions {
            control,
            ..ClientOptions::default()
        },
    )
    .map_err(|error| BridgeError::state(error.to_string()))?;
    if let Some(wake) = wake {
        runtime.set_listener(Arc::new(WakeBridge {
            wake,
            context: WakeContext(wake_context),
        }));
    }
    Ok(PhuxClient::new(Client::from_runtime(
        limits, runtime, history,
    )))
}

/// Feed everything the driver has read since the last poll, then publish the
/// resulting effects. The embedded lane does this inside
/// `phux_client_feed_frame`; a connected client calls this on its wake.
///
/// # Safety
///
/// `client` is exclusively owned for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_poll(client: *mut PhuxClient) -> PhuxClientResult {
    let mut notify_attached = false;
    let result = with_client_mut(client, |client| {
        if !client.is_connected_lane() {
            return Err(BridgeError::state(
                "poll is the connected lane's; feed frames instead",
            ));
        }
        notify_attached = crate::poll_connected(client)?;
        Ok(())
    });
    if result == PhuxClientResult::Ok && notify_attached {
        crate::invoke_attached(client)
    } else {
        result
    }
}

/// Cut a reconnect backoff short, or probe a socket that may have gone
/// stale after the app came to the foreground or the network path changed.
/// A no-op on the embedded lane.
///
/// # Safety
///
/// `client` is exclusively owned for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_nudge(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.runtime.nudge();
        Ok(())
    })
}

/// Drop the socket and redial for fresh snapshots. A no-op on the embedded
/// lane.
///
/// # Safety
///
/// `client` is exclusively owned for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_resync(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.runtime.resync();
        Ok(())
    })
}

/// How many connections this client has opened.
///
/// An embedded client fences per-connection state by building a fresh
/// `PhuxClient` per connection. A connected client cannot: the runtime
/// reconnects underneath a handle that outlives every socket. This is the
/// fence instead. It is zero before the first dial, and every later value
/// retires the state the previous connection built.
///
/// # Safety
///
/// `client` must be a live client for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_connection_epoch(client: *const PhuxClient) -> u64 {
    // SAFETY: checked before dereference.
    unsafe { client.as_ref() }.map_or(0, |client| client.inner.runtime.connection_epoch())
}

/// Whether this client's socket belongs to the runtime.
///
/// # Safety
///
/// `client` must be a live client for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_is_connected(client: *const PhuxClient) -> bool {
    // SAFETY: checked before dereference.
    unsafe { client.as_ref() }.is_some_and(|client| client.inner.is_connected_lane())
}
