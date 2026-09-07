//! Thread-safe socket/readiness boundary for the phux host.
//!
//! Complete protocol frames are allocator-owned queue entries. Only
//! [`wake_payload`] is sent through the native-sdk external channel, keeping
//! frames larger than that channel's 4096-byte effect payload out of it.

const std = @import("std");
const posix = std.posix;

/// Entire native-sdk external-channel payload; frames bypass that channel.
pub const wake_payload = [_]u8{1};
/// Protocol hard bound for one complete frame, including its 4-byte prefix.
pub const max_frame_bytes: usize = 16 * 1024 * 1024 + 4;

pub const max_queued_frames: usize = 128;
pub const max_queued_bytes: usize = 32 * 1024 * 1024;

pub const DisconnectReason = enum { socket_lost, oversized_frame, queue_overflow, stopped };

/// Explicit live/harness connection configuration. The returned slices borrow
/// `spec` and must remain alive while the configuration is in use.
pub const Config = struct {
    host: []const u8,
    port: u16,
    session: []const u8,
    cols: u16 = 80,
    rows: u16 = 24,

    /// Parse deterministic `host:port/session` syntax.
    pub fn parse(spec: []const u8) error{InvalidConfig}!Config {
        const slash = std.mem.indexOfScalar(u8, spec, '/') orelse return error.InvalidConfig;
        const endpoint = spec[0..slash];
        const colon = std.mem.lastIndexOfScalar(u8, endpoint, ':') orelse return error.InvalidConfig;
        const host = endpoint[0..colon];
        const session = spec[slash + 1 ..];
        if (host.len == 0 or session.len == 0) return error.InvalidConfig;
        const port = std.fmt.parseInt(u16, endpoint[colon + 1 ..], 10) catch return error.InvalidConfig;
        if (port == 0) return error.InvalidConfig;
        return .{ .host = host, .port = port, .session = session };
    }
};

const SpinMutex = struct {
    inner: std.atomic.Mutex = .unlocked,

    fn lock(mutex: *SpinMutex) void {
        while (!mutex.inner.tryLock()) std.atomic.spinLoopHint();
    }

    fn unlock(mutex: *SpinMutex) void {
        mutex.inner.unlock();
    }
};

/// One byte means work may be ready. All operations are serialized by the
/// owning queue's mutex: draining before taking frames cannot lose a producer's
/// notification, and closing cannot race a producer writing a reused fd.
const QueueWake = struct {
    fds: [2]posix.fd_t,
    pending: bool = false,

    fn init() !QueueWake {
        var fds: [2]posix.fd_t = undefined;
        if (std.c.pipe(&fds) != 0) return error.WakePipeFailed;
        errdefer for (fds) |fd| {
            _ = std.c.close(fd);
        };
        for (fds) |fd| {
            const flags: posix.O = .{ .NONBLOCK = true };
            if (std.c.fcntl(fd, posix.F.SETFL, @as(c_int, @bitCast(flags))) < 0)
                return error.WakePipeFlagsFailed;
            if (std.c.fcntl(fd, posix.F.SETFD, @as(c_int, posix.FD_CLOEXEC)) < 0)
                return error.WakePipeFlagsFailed;
        }
        return .{ .fds = fds };
    }

    fn deinit(wake: *QueueWake) void {
        for (wake.fds) |fd| _ = std.c.close(fd);
    }

    fn signal(wake: *QueueWake) void {
        if (wake.pending) return;
        // Both ends remain open under the queue lock, and at most one byte is
        // retained. Neither EPIPE nor a full pipe is possible.
        writeToken(wake.fds[1]);
        wake.pending = true;
    }

    fn writeToken(fd: posix.fd_t) void {
        while (true) {
            const count = std.c.write(fd, &wake_payload, wake_payload.len);
            switch (posix.errno(count)) {
                .SUCCESS => {
                    std.debug.assert(count == 1);
                    return;
                },
                .INTR => continue,
                else => unreachable,
            }
        }
    }

    fn drain(wake: *QueueWake) void {
        if (!wake.pending) return;
        var byte: [1]u8 = undefined;
        const count = posix.read(wake.fds[0], &byte) catch unreachable;
        std.debug.assert(count == 1);
        wake.pending = false;
    }
};

/// Owned frame queue crossing between the socket extension and UI owner.
///
/// The queue owns every pending slice. Metadata and payload retention are both
/// bounded: consumed metadata is compacted before it could grow beyond the
/// frame ceiling, while payload accounting is exact under the mutex.
pub const FrameQueue = struct {
    gpa: std.mem.Allocator,
    mutex: SpinMutex = .{},
    pending_bytes: usize = 0,
    disconnect: ?DisconnectReason = null,
    frames: std.ArrayListUnmanaged([]u8) = .empty,
    read_index: usize = 0,
    wake: ?QueueWake = null,

    pub fn init(gpa: std.mem.Allocator) FrameQueue {
        return .{ .gpa = gpa };
    }

    /// The queue must no longer be reachable by a producer or consumer.
    pub fn deinit(queue: *FrameQueue) void {
        queue.disableWake();
        queue.mutex.lock();
        defer queue.mutex.unlock();
        for (queue.frames.items[queue.read_index..]) |frame| queue.gpa.free(frame);
        queue.frames.deinit(queue.gpa);
        queue.frames = .empty;
        queue.read_index = 0;
        queue.pending_bytes = 0;
        queue.disconnect = null;
    }

    /// Copy one complete frame out of ephemeral scratch storage.
    pub fn stage(queue: *FrameQueue, frame: []const u8) bool {
        if (frame.len == 0 or frame.len > max_frame_bytes) {
            queue.markDisconnected(.oversized_frame);
            return false;
        }
        const owned = queue.gpa.dupe(u8, frame) catch {
            queue.markDisconnected(.queue_overflow);
            return false;
        };
        return queue.stageOwned(owned);
    }

    /// Transfer one allocator-owned frame into the queue without copying.
    /// Ownership transfers on every return path; failure frees `frame`.
    pub fn stageOwned(queue: *FrameQueue, frame: []u8) bool {
        if (frame.len == 0 or frame.len > max_frame_bytes) {
            queue.gpa.free(frame);
            queue.markDisconnected(.oversized_frame);
            return false;
        }

        queue.mutex.lock();
        defer queue.mutex.unlock();
        defer queue.signalLocked();

        // Preserve the first disconnect reason until the consumer observes it.
        if (queue.disconnect != null) {
            queue.gpa.free(frame);
            return false;
        }

        const pending_frames = queue.frames.items.len - queue.read_index;
        if (pending_frames == max_queued_frames or
            frame.len > max_queued_bytes - queue.pending_bytes)
        {
            queue.disconnect = .queue_overflow;
            queue.gpa.free(frame);
            return false;
        }

        // A continuously busy queue may never be observed empty. Reclaim its
        // consumed prefix so the metadata backing allocation cannot grow with
        // lifetime rather than with the bounded number of pending frames.
        if (queue.read_index != 0 and queue.frames.items.len >= max_queued_frames) {
            std.mem.copyForwards(
                []u8,
                queue.frames.items[0..pending_frames],
                queue.frames.items[queue.read_index..],
            );
            queue.frames.items.len = pending_frames;
            queue.read_index = 0;
        }

        queue.frames.append(queue.gpa, frame) catch {
            queue.disconnect = .queue_overflow;
            queue.gpa.free(frame);
            return false;
        };
        queue.pending_bytes += frame.len;
        return true;
    }

    /// Transfer one owned frame to the consumer; call `release` after use.
    pub fn take(queue: *FrameQueue) ?[]u8 {
        queue.mutex.lock();
        defer queue.mutex.unlock();
        if (queue.read_index == queue.frames.items.len) {
            queue.frames.clearRetainingCapacity();
            queue.read_index = 0;
            return null;
        }
        const frame = queue.frames.items[queue.read_index];
        queue.read_index += 1;
        queue.pending_bytes -= frame.len;
        return frame;
    }

    pub fn hasPending(queue: *FrameQueue) bool {
        queue.mutex.lock();
        defer queue.mutex.unlock();
        return queue.read_index != queue.frames.items.len;
    }

    pub fn release(queue: *FrameQueue, frame: []u8) void {
        queue.gpa.free(frame);
    }

    /// The first reason wins until `takeDisconnect` or `reset` consumes it.
    pub fn markDisconnected(queue: *FrameQueue, reason: DisconnectReason) void {
        queue.mutex.lock();
        defer queue.mutex.unlock();
        if (queue.disconnect == null) queue.disconnect = reason;
        queue.signalLocked();
    }

    /// Attach before spawning the sole socket consumer. Signal even an empty
    /// queue so frames/disconnects staged before attachment are observed.
    pub fn enableWake(queue: *FrameQueue) !posix.fd_t {
        queue.mutex.lock();
        defer queue.mutex.unlock();
        if (queue.wake != null) return error.WakeAlreadyEnabled;
        queue.wake = try QueueWake.init();
        queue.signalLocked();
        return queue.wake.?.fds[0];
    }

    /// The socket consumer must have joined before its polled fd is closed.
    pub fn disableWake(queue: *FrameQueue) void {
        queue.mutex.lock();
        defer queue.mutex.unlock();
        if (queue.wake) |*wake| wake.deinit();
        queue.wake = null;
    }

    pub fn signalWake(queue: *FrameQueue) void {
        queue.mutex.lock();
        defer queue.mutex.unlock();
        queue.signalLocked();
    }

    fn signalLocked(queue: *FrameQueue) void {
        if (queue.wake) |*wake| wake.signal();
    }

    /// Drain BEFORE flushing, so a stage racing that flush leaves a new token.
    pub fn drainWake(queue: *FrameQueue) void {
        queue.mutex.lock();
        defer queue.mutex.unlock();
        if (queue.wake) |*wake| wake.drain();
    }

    pub fn takeDisconnect(queue: *FrameQueue) ?DisconnectReason {
        queue.mutex.lock();
        defer queue.mutex.unlock();
        const reason = queue.disconnect;
        queue.disconnect = null;
        return reason;
    }

    /// Drop one retired connection while retaining metadata capacity for retry.
    pub fn reset(queue: *FrameQueue) void {
        queue.mutex.lock();
        defer queue.mutex.unlock();
        for (queue.frames.items[queue.read_index..]) |frame| queue.gpa.free(frame);
        queue.frames.clearRetainingCapacity();
        queue.read_index = 0;
        queue.pending_bytes = 0;
        queue.disconnect = null;
    }

    pub fn retainedCapacity(queue: *FrameQueue) usize {
        queue.mutex.lock();
        defer queue.mutex.unlock();
        return queue.frames.capacity;
    }
};

/// Full-duplex staging. The UI drains incoming and fills outgoing; the
/// native-sdk socket worker does the inverse. Neither queue calls client FFI.
pub const Bridge = struct {
    incoming: FrameQueue,
    outgoing: FrameQueue,

    pub fn init(gpa: std.mem.Allocator) Bridge {
        return .{ .incoming = .init(gpa), .outgoing = .init(gpa) };
    }

    /// Stop the worker before calling this method.
    pub fn deinit(bridge: *Bridge) void {
        bridge.incoming.deinit();
        bridge.outgoing.deinit();
    }
};

/// Noninteractive harness using the exact live queues without a socket.
pub const Harness = struct {
    bridge: Bridge,

    pub fn init(gpa: std.mem.Allocator) Harness {
        return .{ .bridge = .init(gpa) };
    }

    pub fn deinit(harness: *Harness) void {
        harness.bridge.deinit();
    }

    pub fn serverFrame(harness: *Harness, frame: []const u8) bool {
        return harness.bridge.incoming.stage(frame);
    }

    pub fn clientFrame(harness: *Harness) ?[]u8 {
        return harness.bridge.outgoing.take();
    }
};

test "explicit config and large-frame channel bypass" {
    const testing = std.testing;
    const config = try Config.parse("127.0.0.1:8022/work");
    try testing.expectEqualStrings("127.0.0.1", config.host);
    try testing.expectEqual(@as(u16, 8022), config.port);
    try testing.expectEqualStrings("work", config.session);
    try testing.expectError(error.InvalidConfig, Config.parse("127.0.0.1/work"));

    var harness = Harness.init(testing.allocator);
    defer harness.deinit();
    const large = try testing.allocator.alloc(u8, 32 * 1024);
    defer testing.allocator.free(large);
    @memset(large, 0xa5);
    try testing.expect(harness.serverFrame(large));
    try testing.expectEqual(@as(usize, 1), wake_payload.len);
    const received = harness.bridge.incoming.take().?;
    defer harness.bridge.incoming.release(received);
    try testing.expectEqualSlices(u8, large, received);
}

test "owned inbound frame transfers without payload allocation" {
    const testing = std.testing;
    var queue = FrameQueue.init(testing.allocator);
    defer queue.deinit();
    const owned = try testing.allocator.dupe(u8, "\x00\x00\x00\x01x");
    const original_ptr = owned.ptr;
    try testing.expect(queue.stageOwned(owned));
    const received = queue.take().?;
    try testing.expectEqual(original_ptr, received.ptr);
    queue.release(received);
}

test "busy queue metadata remains bounded and reusable" {
    const testing = std.testing;
    var queue = FrameQueue.init(testing.allocator);
    defer queue.deinit();

    for (0..max_queued_frames) |_| try testing.expect(queue.stage("frame"));
    for (0..max_queued_frames / 2) |_| queue.release(queue.take().?);
    for (0..max_queued_frames / 2) |_| try testing.expect(queue.stage("again"));
    try testing.expect(queue.frames.items.len <= max_queued_frames);

    while (queue.take()) |frame| queue.release(frame);
    const high_water = queue.retainedCapacity();
    try testing.expect(high_water >= max_queued_frames);
    try testing.expect(queue.stage("reused"));
    queue.release(queue.take().?);
    try testing.expectEqual(high_water, queue.retainedCapacity());
}

test "frame queue overflow is bounded and preserves the first disconnect reason" {
    const testing = std.testing;
    var queue = FrameQueue.init(testing.allocator);
    defer queue.deinit();

    for (0..max_queued_frames) |index| {
        const frame = [_]u8{@intCast(index)};
        try testing.expect(queue.stage(&frame));
    }
    try testing.expectEqual(max_queued_frames, queue.frames.items.len - queue.read_index);
    try testing.expectEqual(max_queued_frames, queue.pending_bytes);

    try testing.expect(!queue.stage("overflow"));
    queue.markDisconnected(.socket_lost);
    try testing.expectEqual(DisconnectReason.queue_overflow, queue.takeDisconnect().?);
    try testing.expect(queue.takeDisconnect() == null);
    try testing.expectEqual(max_queued_frames, queue.frames.items.len - queue.read_index);
    try testing.expectEqual(max_queued_frames, queue.pending_bytes);

    for (0..max_queued_frames) |index| {
        const frame = queue.take().?;
        defer queue.release(frame);
        try testing.expectEqual(@as(u8, @intCast(index)), frame[0]);
    }
    try testing.expect(queue.take() == null);
    try testing.expectEqual(@as(usize, 0), queue.pending_bytes);
    try testing.expect(queue.stage("recovered"));
}

test "frame queue byte ceiling rejects the crossing frame without exceeding retention" {
    const testing = std.testing;
    var queue = FrameQueue.init(testing.allocator);
    defer queue.deinit();

    const full_frames = max_queued_bytes / max_frame_bytes;
    for (0..full_frames) |_| {
        const owned = try testing.allocator.alloc(u8, max_frame_bytes);
        @memset(owned, 0x5a);
        try testing.expect(queue.stageOwned(owned));
    }
    const remaining = max_queued_bytes - queue.pending_bytes;
    try testing.expect(remaining < max_frame_bytes);
    if (remaining != 0) {
        const owned = try testing.allocator.alloc(u8, remaining);
        @memset(owned, 0xa5);
        try testing.expect(queue.stageOwned(owned));
    }
    try testing.expectEqual(max_queued_bytes, queue.pending_bytes);

    try testing.expect(!queue.stage("x"));
    try testing.expectEqual(max_queued_bytes, queue.pending_bytes);
    try testing.expectEqual(DisconnectReason.queue_overflow, queue.takeDisconnect().?);
}

test "every declaration in this module is compiled, not merely reachable" {
    // Zig analyzes only what is referenced, so a module can sit in the build
    // graph with its signatures never checked. See ref.zig.
    @import("phux_ref").refAllDeclsRecursive(@This());
}

test "outgoing wake coalesces bursts and rearms across drain and teardown" {
    var queue = FrameQueue.init(std.testing.allocator);
    defer queue.deinit();
    const fd = try queue.enableWake();
    try std.testing.expectError(error.WakeAlreadyEnabled, queue.enableWake());
    queue.drainWake();
    for (0..max_queued_frames) |_| try std.testing.expect(queue.stage("key"));
    var polls = [_]posix.pollfd{.{ .fd = fd, .events = posix.POLL.IN, .revents = 0 }};
    try std.testing.expectEqual(@as(usize, 1), try posix.poll(&polls, 0));
    queue.drainWake();
    // One drain consumes the whole burst, rather than 128 queued wake bytes.
    try std.testing.expectEqual(@as(usize, 0), try posix.poll(&polls, 0));
    while (queue.take()) |frame| queue.release(frame);
    try std.testing.expect(queue.stage("next"));
    try std.testing.expectEqual(@as(usize, 1), try posix.poll(&polls, 0));
    queue.disableWake();
    try std.testing.expect(queue.stage("detached"));
    queue.signalWake();
    polls[0].fd = try queue.enableWake();
    try std.testing.expectEqual(@as(usize, 1), try posix.poll(&polls, 0));
    queue.drainWake();
    queue.reset();
    try std.testing.expectEqual(@as(usize, 0), try posix.poll(&polls, 0));
}
