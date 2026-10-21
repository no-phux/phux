//! Bounded request correlation and explicit admission for post-ATTACH terminals.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared with the frame dispatcher"
)]

use std::collections::{HashMap, HashSet};
use std::{mem, ptr};

use phux_protocol::ids::{IdempotencyKey, ServerInstance};
use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, InputMode, KillPrecondition, SpawnError, SpawnResource,
    SpawnResult,
};
use phux_protocol::{GroupId, ResourceId, SatelliteHost};

use crate::c::client::Client;
use crate::c::error::{BridgeError, bytes_in, check_struct, terminal_id_in};
use crate::c::{
    ABI_VERSION, PhuxBytes, PhuxClient, PhuxClientResult, PhuxResourceId, bytes_out,
    terminal_id_out, with_client_mut, with_client_ref,
};
use phux_client_core::session::{InputEligibility, KernelSend};

mod close_completion;
mod close_resource;
mod dispatch;
mod queue_attach;
mod queue_spawn;
use close_completion::PendingClose;
pub use close_resource::{
    phux_client_close_tab_resources_supported, phux_client_queue_close_resource,
    phux_client_queue_close_resources, phux_client_queue_close_tab_resources,
};
pub(crate) use dispatch::*;
pub use queue_attach::*;
pub use queue_spawn::*;

pub const MAX_OPERATIONS: usize = 128;
pub const MAX_DYNAMIC_TERMINALS: usize = 256;
pub const MAX_SPAWN_ARGS: usize = 256;
pub const MAX_SPAWN_BYTES: usize = 64 * 1024;
pub const MAX_OPERATION_MESSAGE_BYTES: usize = 4096;

/// A durable creation request. Null owner, empty satellite/cwd, and zero argc mean absent.
///
/// `has_retain_secs` / `retain_secs` and `idempotency_key` are trailing-additive
/// at ABI version 2. A host that still passes the geometry-only size is accepted
/// and treated as both fields absent, so Cockpit Hybrid C keeps compiling and
/// linking without a version bump.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxSpawnOptions {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub owner_terminal: *const PhuxResourceId,
    pub satellite: PhuxBytes,
    pub argv: *const PhuxBytes,
    pub argc: usize,
    pub cwd: PhuxBytes,
    pub cols: u16,
    pub rows: u16,
    /// When true, `retain_secs` is sent as `SPAWN_RESOURCE` field 16 (ADR-0124).
    pub has_retain_secs: bool,
    /// Seconds to keep an exited Terminal. `0` asks for the server default.
    pub retain_secs: u32,
    /// 16-byte spawn key (ADR-0126). All-zero means absent.
    pub idempotency_key: [u8; 16],
}

impl Default for PhuxSpawnOptions {
    fn default() -> Self {
        Self {
            size: mem::size_of::<Self>(),
            version: ABI_VERSION,
            request_id: 0,
            owner_terminal: ptr::null(),
            satellite: PhuxBytes::default(),
            argv: ptr::null(),
            argc: 0,
            cwd: PhuxBytes::default(),
            cols: 80,
            rows: 24,
            has_retain_secs: false,
            retain_secs: 0,
            idempotency_key: [0; 16],
        }
    }
}

/// Size of the geometry-only spawn record (ABI 2 before retain/idempotency).
const fn spawn_options_min_size() -> usize {
    let rows_end = mem::offset_of!(PhuxSpawnOptions, rows) + mem::size_of::<u16>();
    rows_end.next_multiple_of(mem::align_of::<PhuxSpawnOptions>())
}

/// Copy a possibly-shorter host record into the current layout.
///
/// # Safety
/// `ptr` is either null (rejected) or readable for the `size` it declares.
unsafe fn spawn_options_in(ptr: *const PhuxSpawnOptions) -> Result<PhuxSpawnOptions, BridgeError> {
    if ptr.is_null() {
        return Err(BridgeError::invalid("options is null"));
    }
    // SAFETY: `size` is the first field; the caller promised a readable options record.
    let declared = unsafe { ptr::read(ptr.cast::<usize>()) };
    if declared < spawn_options_min_size() {
        return Err(BridgeError::invalid("FFI struct is smaller than required"));
    }
    check_struct(declared, spawn_options_min_size(), {
        // SAFETY: `version` sits immediately after `size` and `declared` covers it.
        unsafe { ptr::read(ptr.byte_add(mem::size_of::<usize>()).cast::<u32>()) }
    })?;
    let mut options = PhuxSpawnOptions::default();
    let copy_len = declared.min(mem::size_of::<PhuxSpawnOptions>());
    // SAFETY: `copy_len` is within both the host record and the local struct.
    unsafe {
        ptr::copy_nonoverlapping(
            ptr.cast::<u8>(),
            ptr::from_mut(&mut options).cast::<u8>(),
            copy_len,
        );
    }
    if declared < mem::size_of::<PhuxSpawnOptions>() {
        options.has_retain_secs = false;
        options.retain_secs = 0;
        options.idempotency_key = [0; 16];
    }
    Ok(options)
}

/// Explicit subscription request; admission is installed before its queued frame is observable.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxAttachResourceOptions {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub terminal_id: PhuxResourceId,
}

impl Default for PhuxAttachResourceOptions {
    fn default() -> Self {
        Self {
            size: mem::size_of::<Self>(),
            version: ABI_VERSION,
            request_id: 0,
            terminal_id: PhuxResourceId::default(),
        }
    }
}

/// Per-terminal subscription withdrawal, with the same sized identity record as attach.
pub type PhuxDetachResourceOptions = PhuxAttachResourceOptions;

/// One completion, distinct from stream READY. Borrowed spans live until the next mutation.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxOperationResult {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    /// 1 = spawn, 2 = attach, 3 = detach, 4 = abandoned-spawn kill, 5 = close, 6 = batch close.
    pub kind: u32,
    /// 1 = success, 2 = refused, 3 = disconnected with unknown outcome. Never retry spawn automatically.
    pub status: u32,
    /// 0 = none, 1 = `SpawnError` wire tag, 2 = `ErrorCode` wire value.
    pub error_domain: u32,
    pub error_code: u32,
    /// id == 0 means no returned terminal. Attach results always retain the requested ID.
    pub terminal_id: PhuxResourceId,
    pub message: PhuxBytes,
}

impl Default for PhuxOperationResult {
    fn default() -> Self {
        Self {
            size: mem::size_of::<Self>(),
            version: ABI_VERSION,
            request_id: 0,
            kind: 0,
            status: 0,
            error_domain: 0,
            error_code: 0,
            terminal_id: PhuxResourceId::default(),
            message: PhuxBytes::default(),
        }
    }
}

#[derive(Clone, Debug)]
enum Pending {
    Spawn {
        satellite: Option<SatelliteHost>,
    },
    Attach(ResourceId),
    Detach(ResourceId),
    /// `KILL_RESOURCE_IF` (ADR-0109): never admits or retires anything.
    Kill(ResourceId),
    /// Intentional termination completes only after acknowledgement and closure.
    Close(PendingClose),
    CloseMany(PendingClose),
    /// `ACQUIRE_INPUT` (ADR-0033): the lease change itself arrives as a
    /// `PHUX_CLIENT_STATUS_INPUT_HOLDER` effect, not through the completion.
    AcquireInput(ResourceId),
    /// `RELEASE_INPUT` (ADR-0033).
    ReleaseInput(ResourceId),
}

impl Pending {
    const fn kind(&self) -> u32 {
        match self {
            Self::Spawn { .. } => 1,
            Self::Attach(_) => 2,
            Self::Detach(_) => 3,
            Self::Kill(_) => 4,
            Self::Close(_) => 5,
            Self::CloseMany(_) => 6,
            Self::AcquireInput(_) => 7,
            Self::ReleaseInput(_) => 8,
        }
    }

    fn terminal(&self) -> Option<ResourceId> {
        match self {
            Self::Spawn { .. } | Self::CloseMany(_) => None,
            Self::Close(close) => close.terminal(),
            Self::Attach(id)
            | Self::Detach(id)
            | Self::Kill(id)
            | Self::AcquireInput(id)
            | Self::ReleaseInput(id) => Some(id.clone()),
        }
    }
}

#[derive(Debug)]
struct Completion {
    request_id: u32,
    kind: u32,
    status: u32,
    error_domain: u32,
    error_code: u32,
    terminal: Option<ResourceId>,
    message: Vec<u8>,
    /// A bound spawn's instance token (`phux_client_operation_instance`).
    instance: Option<[u8; 16]>,
}

#[derive(Default)]
struct DeferredSends {
    history: Option<KernelSend>,
    ack: Option<KernelSend>,
}

#[derive(Default)]
pub(crate) struct Operations {
    pending: HashMap<u32, Pending>,
    completed: Vec<Completion>,
    dynamic: HashSet<ResourceId>,
    /// Bound spawn evidence survives consuming its receipt, but never disconnect.
    instances: HashMap<ResourceId, ServerInstance>,
    // At most one never-transmitted page request and latest cumulative ACK per
    // pending detach, preserving cursor/flow control if withdrawal is refused.
    deferred_sends: HashMap<ResourceId, DeferredSends>,
    last_request_id: u32,
    /// Bridge-internal request ids of a close terminated one terminal at a
    /// time, to the host request they answer.
    parts: HashMap<u32, u32>,
}

impl Operations {
    pub(crate) fn admitted(&self, id: &ResourceId) -> bool {
        self.dynamic.contains(id)
    }

    pub(crate) fn retire(&mut self, id: &ResourceId) {
        self.dynamic.remove(id);
        self.instances.remove(id);
        self.deferred_sends.remove(id);
    }

    pub(crate) fn detaching(&self, id: &ResourceId) -> bool {
        self.pending
            .values()
            .any(|pending| matches!(pending, Pending::Detach(target) if target == id))
    }

    pub(crate) fn fence_send(&mut self, send: &KernelSend) -> bool {
        let id = match send {
            KernelSend::Input { terminal_id, .. } | KernelSend::FrameAck { terminal_id, .. } => {
                terminal_id
            }
            KernelSend::HistoryRequest { key, .. } => &key.terminal_id,
            // Connection-wide, not per-terminal: nothing to fence against a
            // pending detach, so it always sends immediately.
            KernelSend::SubscribeEvents { .. } => return false,
        };
        if !self.detaching(id) {
            return false;
        }
        match send {
            KernelSend::HistoryRequest { .. } => {
                self.deferred_sends.entry(id.clone()).or_default().history = Some(send.clone());
            }
            KernelSend::FrameAck { .. } => {
                self.deferred_sends.entry(id.clone()).or_default().ack = Some(send.clone());
            }
            _ => {}
        }
        true
    }

    pub(crate) fn subscription_pending(&self, id: &ResourceId) -> bool {
        self.pending
            .values()
            .any(|pending| matches!(pending, Pending::Attach(target) | Pending::Detach(target) if target == id))
    }

    fn check_capacity(&self, request_id: u32) -> Result<(), BridgeError> {
        self.check_request_id(request_id)?;
        if self.pending.len() + self.completed.len() >= MAX_OPERATIONS {
            return Err(BridgeError::state(
                "operation queue is full; consume and clear results",
            ));
        }
        Ok(())
    }

    pub(crate) fn check_request_id(&self, request_id: u32) -> Result<(), BridgeError> {
        if request_id <= self.last_request_id || request_id >= crate::c::arrangement::INTERNAL_START
        {
            return Err(BridgeError::invalid(
                "host request IDs must strictly increase in 1..0x7fffffff",
            ));
        }
        Ok(())
    }

    pub(crate) const fn consume_request_id(&mut self, request_id: u32) {
        self.last_request_id = request_id;
    }

    fn check_admission_capacity(&self) -> Result<(), BridgeError> {
        let reserved = self
            .pending
            .values()
            .filter(|op| matches!(op, Pending::Spawn { .. }))
            .count();
        if self.dynamic.len().max(self.instances.len()) + reserved >= MAX_DYNAMIC_TERMINALS {
            return Err(BridgeError::state(
                "dynamic terminal admission limit reached",
            ));
        }
        Ok(())
    }

    fn insert(&mut self, request_id: u32, pending: Pending) {
        self.last_request_id = request_id;
        self.pending.insert(request_id, pending);
    }

    fn pending(&self, request_id: u32) -> Result<&Pending, BridgeError> {
        self.pending
            .get(&request_id)
            .ok_or_else(|| BridgeError::protocol("unsolicited or duplicate operation result"))
    }

    fn complete(
        &mut self,
        request_id: u32,
        status: u32,
        terminal: Option<ResourceId>,
        error_domain: u32,
        error_code: u32,
        message: &str,
    ) {
        self.parts.retain(|_, host| *host != request_id);
        let Some(pending) = self.pending.remove(&request_id) else {
            return;
        };
        self.completed.push(Completion {
            request_id,
            kind: pending.kind(),
            status,
            terminal: terminal.or_else(|| pending.terminal()),
            error_domain,
            error_code,
            message: bounded_message(message),
            instance: None,
        });
    }

    /// Record the instance token a bound spawn's reply carried, on the
    /// completion just recorded for it.
    fn bind_last(&mut self, instance: Option<[u8; 16]>) {
        if let Some(last) = self.completed.last_mut() {
            last.instance = instance;
        }
    }

    pub(crate) fn disconnect(&mut self) {
        let mut ids: Vec<_> = self.pending.keys().copied().collect();
        ids.sort_unstable();
        for request_id in ids {
            self.complete(
                request_id,
                3,
                None,
                0,
                0,
                "connection ended; operation outcome unknown",
            );
        }
        self.dynamic.clear();
        self.instances.clear();
        self.deferred_sends.clear();
        self.parts.clear();
    }
}

fn bounded_message(message: &str) -> Vec<u8> {
    let mut end = message.len().min(MAX_OPERATION_MESSAGE_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.as_bytes()[..end].to_vec()
}

fn valid_terminal(id: &ResourceId) -> Result<(), BridgeError> {
    match id {
        ResourceId::Local { id } if *id > 0 => Ok(()),
        ResourceId::Satellite { host, id }
            if *id > 0
                && !host.as_str().is_empty()
                && host.as_str().len() <= MAX_SPAWN_BYTES
                && !host.as_str().contains('\0') =>
        {
            Ok(())
        }
        _ => Err(BridgeError::invalid(
            "terminal ID must be nonzero with a valid bounded host",
        )),
    }
}

/// `PhuxOperationResult.error_domain` of a refusal an organization command
/// answered: `error_code` is a `PhuxOrgRefusal`.
pub const ERROR_DOMAIN_ORGANIZATION: u32 = 3;

/// Number of retained operation completions (pending operations are excluded).
///
/// # Safety
/// Non-null client must be live and unmodified on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_operation_count(client: *const PhuxClient) -> usize {
    let mut count = 0;
    with_client_ref(client, |client| {
        count = client.operations.completed.len();
        Ok(())
    });
    count
}

/// Read one completion. Caller initializes output size/version; spans are borrowed.
///
/// # Safety
/// Client must be live and unmodified on its owning thread. Output must be readable
/// and writable, disjoint from client storage. Spans expire on the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_operation_get(
    client: *const PhuxClient,
    index: usize,
    out_result: *mut PhuxOperationResult,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies a readable/writable result when non-null.
        let out = unsafe { out_result.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_result is null"))?;
        check_struct(out.size, mem::size_of::<PhuxOperationResult>(), out.version)?;
        *out = PhuxOperationResult::default();
        let result = client
            .operations
            .completed
            .get(index)
            .ok_or_else(|| BridgeError {
                result: PhuxClientResult::NoValue,
                message: String::new(),
            })?;
        *out = PhuxOperationResult {
            request_id: result.request_id,
            kind: result.kind,
            status: result.status,
            error_domain: result.error_domain,
            error_code: result.error_code,
            terminal_id: result
                .terminal
                .as_ref()
                .map_or_else(PhuxResourceId::default, terminal_id_out),
            message: bytes_out(&result.message),
            ..PhuxOperationResult::default()
        };
        Ok(())
    })
}

/// Clear completed results, leaving pending requests and admitted streams intact.
///
/// # Safety
/// Client must be live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_operation_clear(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.operations.completed.clear();
        Ok(())
    })
}

/// Borrow the opaque `HELLO_OK` server identity, retained even after disconnect.
///
/// # Safety
/// Client must be live and unmodified on its owning thread. Output must be writable
/// and disjoint from client storage. Bytes expire at the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_server_id(
    client: *const PhuxClient,
    out_id: *mut PhuxBytes,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies writable output when non-null.
        let out =
            unsafe { out_id.as_mut() }.ok_or_else(|| BridgeError::invalid("out_id is null"))?;
        *out = PhuxBytes::default();
        if client.control().server().is_none() {
            return Err(BridgeError::state(
                "server identity unavailable before HELLO_OK",
            ));
        }
        *out = bytes_out(&client.server_id);
        Ok(())
    })
}

/// Notify transport loss. Pending operations become unknown-outcome results,
/// queued output is discarded, and this client becomes permanently detached.
///
/// # Safety
/// Client must be live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_disconnect(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.detach();
        Ok(())
    })
}

#[cfg(test)]
mod tests;
