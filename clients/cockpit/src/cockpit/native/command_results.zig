//! Read/ack delivery of retained command outcomes. This wire projection owns no
//! execution state; the creation and shared-mutation transactions own results.
const std = @import("std");
const contract = @import("provider_contract");

pub const request_name = "cockpit.command-results";
pub const max_bytes = 74 + 273;
pub const empty = [_]u8{ 1, 0 };
pub const Source = enum(u8) { creation = 1, shared = 2, native_shared = 3 };
pub const Ack = struct { source: Source, command_id: u64 };

/// Wire tags are explicit and independent of the native enums' representation.
pub const Record = struct {
    source: Source,
    command_id: u64,
    connection_epoch: u64,
    request_id: u32,
    mutation_ticket: u64 = 0,
    attachment_request_id: u32 = 0,
    attachment_epoch: u64 = 0,
    placement_request_id: u32 = 0,
    placement_epoch: u64 = 0,
    mutation_outcome: u8 = 0,
    target_session_id: u32 = 0,
    error_domain: u32 = 0,
    error_code: u32 = 0,
    operation: u8,
    placement: u8,
    focus: u8,
    reason: u8 = 0,
    terminal_ref: ?contract.TerminalRef = null,
};

pub fn decodeAck(bytes: []const u8) !?Ack {
    if (std.mem.eql(u8, bytes, &empty)) return null;
    if (bytes.len != 10 or bytes[0] != 1) return error.InvalidAcknowledgement;
    const source = std.enums.fromInt(Source, bytes[1]) orelse return error.InvalidAcknowledgement;
    const id = std.mem.readInt(u64, bytes[2..10], .little);
    if (id == 0) return error.InvalidAcknowledgement;
    return .{ .source = source, .command_id = id };
}

pub fn encode(record: Record, buffer: *[max_bytes]u8) []const u8 {
    buffer[0] = 1;
    buffer[1] = @intFromEnum(record.source);
    buffer[2] = record.operation;
    buffer[3] = record.placement;
    buffer[4] = record.focus;
    buffer[5] = record.reason;
    std.mem.writeInt(u64, buffer[6..14], record.command_id, .little);
    std.mem.writeInt(u64, buffer[14..22], record.connection_epoch, .little);
    std.mem.writeInt(u32, buffer[22..26], record.request_id, .little);
    std.mem.writeInt(u64, buffer[26..34], record.mutation_ticket, .little);
    std.mem.writeInt(u32, buffer[34..38], record.attachment_request_id, .little);
    std.mem.writeInt(u32, buffer[38..42], record.placement_request_id, .little);
    std.mem.writeInt(u32, buffer[42..46], record.error_domain, .little);
    std.mem.writeInt(u32, buffer[46..50], record.error_code, .little);
    std.mem.writeInt(u64, buffer[50..58], record.attachment_epoch, .little);
    std.mem.writeInt(u64, buffer[58..66], record.placement_epoch, .little);
    buffer[66] = record.mutation_outcome;
    buffer[67] = 0;
    std.mem.writeInt(u32, buffer[68..72], record.target_session_id, .little);
    const length = if (record.terminal_ref) |ref| encodeTerminal(ref, buffer[74..]) else 0;
    std.mem.writeInt(u16, buffer[72..74], @intCast(length), .little);
    return buffer[0 .. 74 + length];
}

pub fn encodeResult(source: Source, result: anytype, buffer: *[max_bytes]u8) []const u8 {
    return encode(.{
        .source = source,
        .command_id = result.command_id,
        .connection_epoch = result.connection_epoch,
        .request_id = result.request_id,
        .mutation_ticket = result.mutation_ticket,
        .attachment_request_id = result.attach_request_id,
        .attachment_epoch = result.attach_connection_epoch,
        .placement_request_id = result.placement_request_id,
        .placement_epoch = result.placement_connection_epoch,
        .mutation_outcome = if (result.mutation_outcome) |outcome| operationTag(outcome) else 0,
        .target_session_id = result.target_session_id,
        .error_domain = result.error_domain,
        .error_code = result.error_code,
        .operation = operationTag(result.operation),
        .placement = placementTag(result.placement),
        .focus = focusTag(result.focus),
        .reason = @intCast(@intFromEnum(result.reason)),
        .terminal_ref = result.terminal_ref,
    }, buffer);
}

fn operationTag(operation: anytype) u8 {
    return switch (operation) {
        .success => 1,
        .refused => 2,
        .unknown => 3,
    };
}

fn placementTag(placement: anytype) u8 {
    return switch (placement) {
        .placed => 1,
        .refused => 2,
        .destination_lost => 3,
        .unknown => 4,
        .not_requested => 5,
    };
}

fn focusTag(focus: anytype) u8 {
    return switch (focus) {
        .focused => 1,
        .superseded => 2,
        .not_requested => 3,
    };
}

fn encodeTerminal(ref: contract.TerminalRef, buffer: []u8) usize {
    std.mem.writeInt(u64, buffer[0..8], @intFromEnum(ref.provider_id), .little);
    switch (ref.terminal_id) {
        .local => |id| {
            buffer[8] = 1;
            std.mem.writeInt(u64, buffer[9..17], @intFromEnum(id), .little);
            return 17;
        },
        .phux => |id| {
            buffer[8] = 2;
            std.mem.writeInt(u32, buffer[9..13], id.kind, .little);
            std.mem.writeInt(u32, buffer[13..17], id.id, .little);
            buffer[17] = id.host_len;
            @memcpy(buffer[18 .. 18 + id.host_len], id.host_storage[0..id.host_len]);
            return 18 + @as(usize, id.host_len);
        },
    }
}

test "result delivery preserves full correlation and maximum remote identity" {
    const host: [255]u8 = @splat('h');
    const remote = try contract.RemoteResourceId.fromPhux(1, 0xfedcba98, &host);
    var buffer: [max_bytes]u8 = undefined;
    const bytes = encode(.{
        .source = .creation,
        .command_id = 0xfedcba9876543210,
        .connection_epoch = 0xabcdef0123456789,
        .request_id = 0xffffffff,
        .mutation_ticket = 0x9876543210fedcba,
        .attachment_request_id = 0xfffffffe,
        .placement_request_id = 0xfffffffd,
        .operation = 1,
        .placement = 3,
        .focus = 2,
        .terminal_ref = .{ .provider_id = .phux, .terminal_id = .{ .phux = remote } },
    }, &buffer);
    try std.testing.expectEqual(max_bytes, bytes.len);
    try std.testing.expectEqual(@as(u64, 0xfedcba9876543210), std.mem.readInt(u64, bytes[6..14], .little));
    try std.testing.expectEqual(@as(u64, 0xabcdef0123456789), std.mem.readInt(u64, bytes[14..22], .little));
    try std.testing.expectEqual(@as(u32, 0xfffffffe), std.mem.readInt(u32, bytes[34..38], .little));
    try std.testing.expectEqual(@as(u32, 0xfffffffd), std.mem.readInt(u32, bytes[38..42], .little));
    try std.testing.expectEqual(@as(u32, 0xffffffff), std.mem.readInt(u32, bytes[22..26], .little));
    try std.testing.expectEqual(@as(u64, 0x9876543210fedcba), std.mem.readInt(u64, bytes[26..34], .little));
    try std.testing.expectEqual(@as(u16, 273), std.mem.readInt(u16, bytes[72..74], .little));
    try std.testing.expectEqual(@intFromEnum(contract.ProviderId.phux), std.mem.readInt(u64, bytes[74..82], .little));
    try std.testing.expectEqual(@as(u8, 2), bytes[82]);
    try std.testing.expectEqual(@as(u32, 1), std.mem.readInt(u32, bytes[83..87], .little));
    try std.testing.expectEqual(@as(u32, 0xfedcba98), std.mem.readInt(u32, bytes[87..91], .little));
    try std.testing.expectEqual(@as(u8, 255), bytes[91]);
    try std.testing.expectEqualSlices(u8, &host, bytes[92..]);
}

test "result terminal identity preserves local provider and full resource ID" {
    var buffer: [273]u8 = undefined;
    const length = encodeTerminal(.{
        .provider_id = .local,
        .terminal_id = .{ .local = @enumFromInt(0xabcdef0123456789) },
    }, &buffer);
    try std.testing.expectEqual(@as(usize, 17), length);
    try std.testing.expectEqual(@intFromEnum(contract.ProviderId.local), std.mem.readInt(u64, buffer[0..8], .little));
    try std.testing.expectEqual(@as(u8, 1), buffer[8]);
    try std.testing.expectEqual(@as(u64, 0xabcdef0123456789), std.mem.readInt(u64, buffer[9..17], .little));
}

test "result acknowledgement is exact and origin qualified" {
    try std.testing.expect(try decodeAck(&empty) == null);
    var bytes = [_]u8{ 1, 3, 0, 0, 0, 0, 0, 0, 0, 0 };
    try std.testing.expectError(error.InvalidAcknowledgement, decodeAck(&bytes));
    std.mem.writeInt(u64, bytes[2..10], 0xfedcba9876543210, .little);
    const ack = (try decodeAck(&bytes)).?;
    try std.testing.expectEqual(.native_shared, ack.source);
    try std.testing.expectEqual(@as(u64, 0xfedcba9876543210), ack.command_id);
    try std.testing.expectError(error.InvalidAcknowledgement, decodeAck(bytes[0..9]));
    bytes[1] = 4;
    try std.testing.expectError(error.InvalidAcknowledgement, decodeAck(&bytes));
}
