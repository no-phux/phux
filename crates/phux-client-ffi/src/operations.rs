//! Bounded request correlation and explicit admission for post-ATTACH terminals.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared with the frame dispatcher"
)]

use std::collections::{HashMap, HashSet};
use std::{mem, ptr};

use phux_protocol::wire::frame::{Command, CommandResult, FrameKind, SpawnError, SpawnResult};
use phux_protocol::{GroupId, SatelliteHost, TerminalId};

use crate::client::Client;
use crate::error::{BridgeError, bytes_in, check_struct, terminal_id_in};
use crate::{
    ABI_VERSION, PhuxBytes, PhuxClient, PhuxClientResult, PhuxTerminalId, bytes_out,
    terminal_id_out, with_client_mut, with_client_ref,
};

pub const MAX_OPERATIONS: usize = 128;
pub const MAX_DYNAMIC_TERMINALS: usize = 256;
pub const MAX_SPAWN_ARGS: usize = 256;
pub const MAX_SPAWN_BYTES: usize = 64 * 1024;
pub const MAX_OPERATION_MESSAGE_BYTES: usize = 4096;

/// A durable creation request. Null owner, empty satellite/cwd, and zero argc mean absent.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxSpawnOptions {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub owner_terminal: *const PhuxTerminalId,
    pub satellite: PhuxBytes,
    pub argv: *const PhuxBytes,
    pub argc: usize,
    pub cwd: PhuxBytes,
    pub cols: u16,
    pub rows: u16,
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
        }
    }
}

/// Explicit subscription request; admission is installed before its queued frame is observable.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxAttachTerminalOptions {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub terminal_id: PhuxTerminalId,
}

impl Default for PhuxAttachTerminalOptions {
    fn default() -> Self {
        Self {
            size: mem::size_of::<Self>(),
            version: ABI_VERSION,
            request_id: 0,
            terminal_id: PhuxTerminalId::default(),
        }
    }
}

/// Per-terminal subscription withdrawal, with the same sized identity record as attach.
pub type PhuxDetachTerminalOptions = PhuxAttachTerminalOptions;

/// One completion, distinct from stream READY. Borrowed spans live until the next mutation.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxOperationResult {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    /// 1 = spawn, 2 = attach terminal, 3 = detach terminal.
    pub kind: u32,
    /// 1 = success, 2 = refused, 3 = disconnected with unknown outcome. Never retry spawn automatically.
    pub status: u32,
    /// 0 = none, 1 = `SpawnError` wire tag, 2 = `ErrorCode` wire value.
    pub error_domain: u32,
    pub error_code: u32,
    /// id == 0 means no returned terminal. Attach results always retain the requested ID.
    pub terminal_id: PhuxTerminalId,
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
            terminal_id: PhuxTerminalId::default(),
            message: PhuxBytes::default(),
        }
    }
}

#[derive(Clone, Debug)]
enum Pending {
    Spawn { satellite: Option<SatelliteHost> },
    Attach(TerminalId),
    Detach(TerminalId),
}

impl Pending {
    const fn kind(&self) -> u32 {
        match self {
            Self::Spawn { .. } => 1,
            Self::Attach(_) => 2,
            Self::Detach(_) => 3,
        }
    }

    fn terminal(&self) -> Option<TerminalId> {
        match self {
            Self::Spawn { .. } => None,
            Self::Attach(id) | Self::Detach(id) => Some(id.clone()),
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
    terminal: Option<TerminalId>,
    message: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct Operations {
    pending: HashMap<u32, Pending>,
    completed: Vec<Completion>,
    dynamic: HashSet<TerminalId>,
    last_request_id: u32,
}

impl Operations {
    pub(crate) fn admitted(&self, id: &TerminalId) -> bool {
        self.dynamic.contains(id)
    }

    pub(crate) fn retire(&mut self, id: &TerminalId) {
        self.dynamic.remove(id);
    }

    fn subscription_pending(&self, id: &TerminalId) -> bool {
        self.pending
            .values()
            .any(|pending| matches!(pending, Pending::Attach(target) | Pending::Detach(target) if target == id))
    }

    fn check_capacity(&self, request_id: u32) -> Result<(), BridgeError> {
        if request_id <= self.last_request_id {
            return Err(BridgeError::invalid(
                "operation request IDs must be nonzero and strictly increasing",
            ));
        }
        if self.pending.len() + self.completed.len() >= MAX_OPERATIONS {
            return Err(BridgeError::state(
                "operation queue is full; consume and clear results",
            ));
        }
        Ok(())
    }

    fn check_admission_capacity(&self) -> Result<(), BridgeError> {
        let reserved = self
            .pending
            .values()
            .filter(|op| matches!(op, Pending::Spawn { .. }))
            .count();
        if self.dynamic.len() + reserved >= MAX_DYNAMIC_TERMINALS {
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
        terminal: Option<TerminalId>,
        error_domain: u32,
        error_code: u32,
        message: &str,
    ) {
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
        });
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
    }
}

fn bounded_message(message: &str) -> Vec<u8> {
    let mut end = message.len().min(MAX_OPERATION_MESSAGE_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.as_bytes()[..end].to_vec()
}

fn valid_terminal(id: &TerminalId) -> Result<(), BridgeError> {
    match id {
        TerminalId::Local { id } if *id > 0 => Ok(()),
        TerminalId::Satellite { host, id }
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

unsafe fn text_in(value: PhuxBytes, budget: &mut usize) -> Result<String, BridgeError> {
    *budget = budget
        .checked_sub(value.len)
        .ok_or_else(|| BridgeError::invalid("spawn text exceeds 64 KiB aggregate bound"))?;
    // SAFETY: caller provides a readable span; its size was bounded above.
    let bytes = unsafe { bytes_in(value.data, value.len) }?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| BridgeError::invalid("spawn text is not UTF-8"))?;
    if text.contains('\0') {
        return Err(BridgeError::invalid("spawn text contains NUL"));
    }
    Ok(text.to_owned())
}

unsafe fn command_in(
    options: &PhuxSpawnOptions,
    budget: &mut usize,
) -> Result<Option<Vec<String>>, BridgeError> {
    if options.argc == 0 {
        return Ok(None);
    }
    if options.argc > MAX_SPAWN_ARGS || options.argv.is_null() {
        return Err(BridgeError::invalid(
            "argv is null or exceeds 256 arguments",
        ));
    }
    // SAFETY: caller promises argc readable PhuxBytes records; count is bounded.
    let args = unsafe { std::slice::from_raw_parts(options.argv, options.argc) };
    let command: Vec<_> = args
        .iter()
        .map(|value| {
            // SAFETY: argument spans obey the options contract.
            unsafe { text_in(*value, budget) }
        })
        .collect::<Result<_, _>>()?;
    if command[0].is_empty() {
        return Err(BridgeError::invalid("spawn executable is empty"));
    }
    Ok(Some(command))
}

unsafe fn spawn_frame(options: &PhuxSpawnOptions) -> Result<FrameKind, BridgeError> {
    check_struct(
        options.size,
        mem::size_of::<PhuxSpawnOptions>(),
        options.version,
    )?;
    if options.cols == 0 || options.rows == 0 {
        return Err(BridgeError::invalid("spawn geometry must be nonzero"));
    }
    let mut budget = MAX_SPAWN_BYTES;
    // SAFETY: caller's options contract covers all spans.
    let satellite = unsafe { text_in(options.satellite, &mut budget) }?;
    // SAFETY: caller's options contract covers all spans.
    let cwd = unsafe { text_in(options.cwd, &mut budget) }?;
    // SAFETY: caller's options contract covers the argv array and spans.
    let command = unsafe { command_in(options, &mut budget) }?;
    // SAFETY: caller's options contract covers the optional owner pointer.
    let owner_terminal = unsafe { owner_in(options.owner_terminal, &satellite, &mut budget) }?;
    Ok(FrameKind::SpawnTerminal {
        request_id: options.request_id,
        group: GroupId::new(1),
        command,
        cwd: (!cwd.is_empty()).then_some(cwd),
        env: None,
        term: None,
        satellite: (!satellite.is_empty()).then(|| SatelliteHost::new(satellite)),
        owner_terminal,
        agent_session: None,
        initial_size: Some((options.cols, options.rows)),
    })
}

unsafe fn owner_in(
    owner: *const PhuxTerminalId,
    satellite: &str,
    budget: &mut usize,
) -> Result<Option<TerminalId>, BridgeError> {
    if owner.is_null() {
        return Ok(None);
    }
    // SAFETY: caller supplies a readable owner ID.
    let owner = unsafe { terminal_id_in(owner) }?;
    valid_terminal(&owner)?;
    let expected_route = match &owner {
        TerminalId::Local { .. } => "",
        TerminalId::Satellite { host, .. } => {
            *budget = budget
                .checked_sub(host.as_str().len())
                .ok_or_else(|| BridgeError::invalid("spawn text exceeds 64 KiB aggregate bound"))?;
            host.as_str()
        }
    };
    if satellite != expected_route {
        return Err(BridgeError::invalid(
            "spawn owner must be local without a route, or satellite with an exactly matching route",
        ));
    }
    Ok(Some(owner))
}

/// Queue one durable spawn; consumes no request ID on local validation failure.
///
/// # Safety
/// Client must be live, exclusively accessed on its owning thread. Options and
/// its non-null owner, argc argv records, and nonempty spans must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_spawn(
    client: *mut PhuxClient,
    options: *const PhuxSpawnOptions,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        // SAFETY: caller supplies readable options when non-null.
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        // SAFETY: forwards the options contract.
        let frame = unsafe { spawn_frame(options) }?;
        ensure_queue_capacity(client, options.request_id)?;
        client.operations.check_admission_capacity()?;
        let FrameKind::SpawnTerminal { satellite, .. } = &frame else {
            unreachable!()
        };
        let pending = Pending::Spawn {
            satellite: satellite.clone(),
        };
        client.queue_frame(&frame)?;
        client.operations.insert(options.request_id, pending);
        Ok(())
    })
}

/// Queue a subscription and admit bootstrap frames before `COMMAND_RESULT` arrives.
///
/// # Safety
/// Client must be live and exclusively accessed on its owning thread. Options and
/// any nonempty terminal host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_attach_terminal(
    client: *mut PhuxClient,
    options: *const PhuxAttachTerminalOptions,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        // SAFETY: caller supplies readable options when non-null.
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        check_struct(
            options.size,
            mem::size_of::<PhuxAttachTerminalOptions>(),
            options.version,
        )?;
        // SAFETY: options includes the readable ID and its host span.
        let id = unsafe { terminal_id_in(ptr::from_ref(&options.terminal_id)) }?;
        valid_terminal(&id)?;
        queue_terminal_attach(client, options.request_id, id)
    })
}

fn queue_terminal_attach(
    client: &mut Client,
    request_id: u32,
    id: TerminalId,
) -> Result<(), BridgeError> {
    ensure_queue_capacity(client, request_id)?;
    client.operations.check_admission_capacity()?;
    if client.session.active_attach_contains(&id) || client.operations.admitted(&id) {
        return Err(BridgeError::state("terminal is already admitted"));
    }
    if client.operations.subscription_pending(&id) {
        return Err(BridgeError::state(
            "terminal already has a pending subscription operation",
        ));
    }
    if client.session.input_eligibility(&id)
        == phux_client_core::session::InputEligibility::Ineligible(
            phux_client_core::session::InputBlockReason::Closed,
        )
    {
        return Err(BridgeError::state(
            "terminal in the initial ATTACH inventory has closed",
        ));
    }
    client.queue_frame(&FrameKind::Command {
        request_id,
        command: Command::AttachTerminal {
            terminal_id: id.clone(),
        },
    })?;
    client.operations.dynamic.insert(id.clone());
    client.operations.insert(request_id, Pending::Attach(id));
    Ok(())
}

/// Queue per-terminal detach without stopping the remote process. Admission and
/// replica state remain until correlated success; refusals preserve both.
///
/// # Safety
/// Client is live and exclusively accessed. Options and terminal host are readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_detach_terminal(
    client: *mut PhuxClient,
    options: *const PhuxDetachTerminalOptions,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        // SAFETY: caller supplies readable options when non-null.
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        check_struct(
            options.size,
            mem::size_of::<PhuxDetachTerminalOptions>(),
            options.version,
        )?;
        // SAFETY: options includes the readable ID and host span.
        let id = unsafe { terminal_id_in(ptr::from_ref(&options.terminal_id)) }?;
        valid_terminal(&id)?;
        queue_terminal_detach(client, options.request_id, id)
    })
}

fn queue_terminal_detach(
    client: &mut Client,
    request_id: u32,
    id: TerminalId,
) -> Result<(), BridgeError> {
    ensure_queue_capacity(client, request_id)?;
    if client.operations.subscription_pending(&id) {
        return Err(BridgeError::state(
            "terminal has a pending subscription operation",
        ));
    }
    if !client.operations.admitted(&id) && !client.session.active_attach_contains(&id) {
        return Err(BridgeError::state("terminal is not admitted"));
    }
    client.queue_frame(&FrameKind::Command {
        request_id,
        command: Command::DetachTerminal {
            terminal_id: id.clone(),
        },
    })?;
    client.operations.insert(request_id, Pending::Detach(id));
    Ok(())
}

fn ensure_queue_capacity(client: &Client, request_id: u32) -> Result<(), BridgeError> {
    client.operations.check_capacity(request_id)?;
    if client.outgoing.len() >= MAX_OPERATIONS {
        return Err(BridgeError::state(
            "outgoing queue is full; drain outgoing frames before adding operations",
        ));
    }
    Ok(())
}

pub(crate) fn dispatch(
    client: &mut Client,
    frame: FrameKind,
) -> Result<Option<FrameKind>, BridgeError> {
    match frame {
        FrameKind::TerminalSpawned { request_id, result } => {
            complete_spawn(client, request_id, result)?;
        }
        FrameKind::CommandResult { request_id, result } => {
            complete_subscription(client, request_id, result)?;
        }
        FrameKind::Error {
            request_id: Some(request_id),
            code,
            message,
        } => {
            client.operations.pending(request_id)?;
            refuse_operation(client, request_id, 2, u32::from(code.as_wire()), &message)?;
        }
        other => return Ok(Some(other)),
    }
    Ok(None)
}

fn complete_spawn(
    client: &mut Client,
    request_id: u32,
    result: SpawnResult,
) -> Result<(), BridgeError> {
    let Pending::Spawn { satellite } = client.operations.pending(request_id)? else {
        return Err(BridgeError::protocol(
            "TERMINAL_SPAWNED does not answer a spawn",
        ));
    };
    match result {
        SpawnResult::Ok(id) => {
            validate_spawn_reply(client, &id, satellite.as_ref())?;
            if matches!(id, TerminalId::Local { .. }) {
                client.operations.dynamic.insert(id.clone());
            }
            client
                .operations
                .complete(request_id, 1, Some(id), 0, 0, "");
        }
        SpawnResult::Err(error) => {
            let (code, message) = spawn_error(error);
            client
                .operations
                .complete(request_id, 2, None, 1, code, &message);
        }
        _ => return Err(BridgeError::protocol("unknown spawn result")),
    }
    Ok(())
}

fn validate_spawn_reply(
    client: &Client,
    id: &TerminalId,
    satellite: Option<&SatelliteHost>,
) -> Result<(), BridgeError> {
    valid_terminal(id).map_err(|error| BridgeError::protocol(error.message))?;
    let host = match id {
        TerminalId::Local { .. } => None,
        TerminalId::Satellite { host, .. } => Some(host),
    };
    if host != satellite {
        return Err(BridgeError::protocol(
            "spawn reply host differs from request",
        ));
    }
    if client.session.active_attach_contains(id) || client.operations.admitted(id) {
        return Err(BridgeError::protocol(
            "spawn reply reused an admitted terminal ID",
        ));
    }
    Ok(())
}

fn spawn_error(error: SpawnError) -> (u32, String) {
    match error {
        SpawnError::GroupNotFound => (0, "group not found".into()),
        SpawnError::SpawnFailed(message) => (1, message),
        SpawnError::UnsupportedSatelliteRoute => (2, "unsupported satellite route".into()),
        SpawnError::SatelliteUnreachable(message) => (3, message),
        _ => (u32::MAX, "unknown spawn refusal".into()),
    }
}

fn complete_subscription(
    client: &mut Client,
    request_id: u32,
    result: CommandResult,
) -> Result<(), BridgeError> {
    if matches!(
        client.operations.pending(request_id)?,
        Pending::Spawn { .. }
    ) {
        return Err(BridgeError::protocol(
            "COMMAND_RESULT does not answer a terminal subscription operation",
        ));
    }
    match result {
        CommandResult::Ok => {
            if let Pending::Detach(id) = client.operations.pending(request_id)?.clone() {
                complete_detach(client, &id)?;
            }
            client.operations.complete(request_id, 1, None, 0, 0, "");
        }
        CommandResult::Error { code, message } => {
            refuse_operation(client, request_id, 2, u32::from(code.as_wire()), &message)?;
        }
        _ => {
            return Err(BridgeError::protocol(
                "unexpected terminal subscription result value",
            ));
        }
    }
    Ok(())
}

fn complete_detach(client: &mut Client, id: &TerminalId) -> Result<(), BridgeError> {
    client.operations.retire(id);
    if !client.session.detach_terminal(id) {
        return Err(BridgeError::state("cannot detach before ATTACH barrier"));
    }
    crate::forget_terminal(client, id);
    client
        .owned_effects
        .push(crate::OwnedEffect::simple(1, 3, id.clone()));
    client.publish_effects();
    Ok(())
}

fn refuse_operation(
    client: &mut Client,
    request_id: u32,
    domain: u32,
    code: u32,
    message: &str,
) -> Result<(), BridgeError> {
    if let Pending::Attach(id) = client.operations.pending(request_id)?.clone() {
        // A bootstrap may already have published before refusal. Revoke only this operation's admission.
        let published = client.session.published(&id).is_some();
        release_terminal(client, &id)?;
        crate::forget_terminal(client, &id);
        if published {
            client
                .owned_effects
                .push(crate::OwnedEffect::simple(1, 3, id));
            client.publish_effects();
        }
    }
    client
        .operations
        .complete(request_id, 2, None, domain, code, message);
    Ok(())
}

/// The explicit admission gate replaces permanent kernel death records for
/// dynamic subscriptions. Initial ATTACH participants retain the kernel barrier.
pub(crate) fn release_terminal(client: &mut Client, id: &TerminalId) -> Result<(), BridgeError> {
    if !client.operations.admitted(id) {
        return Ok(());
    }
    client.operations.retire(id);
    if !client.session.release_terminal(id) {
        return Err(BridgeError::state(
            "cannot release an initial ATTACH participant",
        ));
    }
    Ok(())
}

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
                .map_or_else(PhuxTerminalId::default, terminal_id_out),
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
        if !client.protocol_ready {
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
