//! Owned command outcomes. Acceptance is independent of terminal READY.
const provider = @import("provider_contract");

pub const Kind = enum(u32) { spawn = 1, attach = 2, detach = 3 };
pub const Status = enum(u32) { success = 1, refused = 2, unknown_outcome = 3 };
pub const ErrorDomain = enum(u32) { none = 0, spawn = 1, protocol = 2 };

pub const Result = struct {
    request_id: u32,
    connection_epoch: u64,
    kind: Kind,
    status: Status,
    terminal_ref: ?provider.TerminalRef = null,
    error_domain: ErrorDomain = .none,
    error_code: u32 = 0,
    // The ABI bounds operation messages at 4096 bytes. Inline storage makes
    // disconnect finalization infallible and results independent of FFI clear.
    message_storage: [4096]u8 = undefined,
    message_len: usize = 0,

    pub fn message(result: *const Result) []const u8 {
        return result.message_storage[0..result.message_len];
    }
};
