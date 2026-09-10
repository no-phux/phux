//! Owned terminal command evidence. Admission is returned by the caller; these
//! values describe eventual execution independently from optional presentation.
const contract = @import("provider_contract");

pub const Operation = enum { success, refused, unknown };
pub const Placement = enum { placed, refused, destination_lost, unknown, not_requested };
pub const Focus = enum { focused, superseded, not_requested };
pub const Origin = enum { ui, native };
pub const Reason = enum {
    completed,
    operation_refused,
    operation_unknown,
    disconnected,
    context_changed,
    missing_identity,
    identity_mismatch,
    attach_refused,
    attach_unknown,
    attach_unavailable,
    terminal_lost,
    publication_failed,
    destination_lost,
    workspace_unavailable,
    operation_capacity,
    stale_target,
    mutation_refused,
    mutation_not_confirmed,
    mutation_unknown,
    lost_completion,
    competing_topology,
    projection_refused,
};

pub const Result = struct {
    command_id: u64,
    origin: Origin = .ui,
    /// Original execution request, never overwritten by follow-up attachment.
    /// Zero denotes live admission without a provider execution request.
    request_id: u32,
    connection_epoch: u64,
    terminal_ref: ?contract.TerminalRef = null,
    placement_request_id: u32 = 0,
    placement_connection_epoch: u64 = 0,
    mutation_ticket: u64 = 0,
    attach_request_id: u32 = 0,
    attach_connection_epoch: u64 = 0,
    error_domain: u32 = 0,
    error_code: u32 = 0,
    destination_window: ?usize = null,
    destination_window_epoch: u64 = 0,
    shared_window_id: ?contract.workspace.WindowId = null,
    operation: Operation,
    placement: Placement,
    focus: Focus = .not_requested,
    reason: Reason,
};
