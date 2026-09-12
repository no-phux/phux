//! Native-sdk socket extension for phux length framing.
//!
//! This worker owns only the socket. Client state and client FFI remain on the
//! deterministic UI thread. Complete frames cross `Bridge`; only a one-byte
//! wake crosses `ChannelHandle`.

const std = @import("std");
const builtin = @import("builtin");
const native_sdk = @import("native_sdk");
const transport = @import("phux_transport");
pub const startup = @import("startup.zig");
/// phux-client-ffi's remote-host tunnel. The provider reaches it through this
/// re-export so the file belongs to exactly one build module.
pub const remote = @import("remote_tunnel.zig");
const posix = std.posix;
const max_flush_batch: usize = 16;

test {
    _ = remote;
}

/// Endpoint slices are borrowed and must remain alive until `Worker.stop` has
/// returned.
pub const Endpoint = union(enum) {
    tcp: struct { host: []const u8, port: u16 },
    unix: []const u8,
    /// A host in the phux CLI's `[[remote]]` registry, dialed through a
    /// phux-client-ffi tunnel. Framing above the socket is unchanged.
    remote: Remote,

    pub const Remote = struct {
        /// Registry name or `[USER@]HOST[:PORT]`.
        target: []const u8,
        /// Absolute phux `config.toml`, or empty for the CLI's own path.
        config_path: []const u8 = "",
        /// Provider-owned failure record; outlives every worker.
        status: ?*remote.Status = null,
    };
};

const max_resolved_addresses = 16;
const ResolvedAddresses = struct {
    items: [max_resolved_addresses]std.Io.net.IpAddress = undefined,
    len: usize = 0,
};

/// DNS is isolated because the platform resolver is blocking. This detached
/// context owns everything it reads and frees itself if cancellation wins.
const ResolveContext = struct {
    host: [std.Io.net.HostName.max_len:0]u8 = @splat(0),
    host_len: usize,
    port: u16,
    // 0 = resolving, 1 = complete, 2 = abandoned by the worker.
    state: std.atomic.Value(u8) = .init(0),
    result: ResolvedAddresses = .{},

    fn run(context: *ResolveContext) void {
        context.result = resolveBlocking(context.host[0..context.host_len :0], context.port);
        const previous = context.state.cmpxchgStrong(0, 1, .release, .acquire);
        if (previous == 2) std.heap.page_allocator.destroy(context);
    }
};

pub const Worker = struct {
    gpa: std.mem.Allocator,
    io: std.Io,
    bridge: *transport.Bridge,
    handle: native_sdk.ChannelHandle,
    endpoint: Endpoint,
    startup_options: startup.Options = .{},
    /// The far end of a remote endpoint's socket pair. Freed by `stop` after
    /// the join, so its failure reason stays readable until then.
    tunnel: ?remote.Tunnel = null,
    thread: ?std.Thread = null,
    outgoing_wake_fd: posix.fd_t = -1,
    stopping: std.atomic.Value(bool) = std.atomic.Value(bool).init(false),

    // The lock prevents a descriptor from being closed and reused between a
    // stop-side load and shutdown. It is held only for publish/shutdown/close.
    fd_mutex: std.atomic.Mutex = .unlocked,
    fd: posix.fd_t = -1,

    pub fn start(
        io: std.Io,
        gpa: std.mem.Allocator,
        bridge: *transport.Bridge,
        handle: native_sdk.ChannelHandle,
        endpoint: Endpoint,
    ) !*Worker {
        return startWithOptions(io, gpa, bridge, handle, endpoint, .{});
    }

    /// Options borrow their path until stop returns; fixtures must explicitly
    /// name their helper rather than invoking the user's installed coordinator.
    pub fn startWithOptions(
        io: std.Io,
        gpa: std.mem.Allocator,
        bridge: *transport.Bridge,
        handle: native_sdk.ChannelHandle,
        endpoint: Endpoint,
        options: startup.Options,
    ) !*Worker {
        return startOwned(io, gpa, bridge, handle, endpoint, options, null);
    }

    /// Adopts the exact registry-checked tunnel, including its endpoint, pin and
    /// config/token provenance. No registry lookup occurs on this path. Ownership
    /// transfers on EVERY return, including allocation, wake and thread failure.
    /// The caller relinquishes the tunnel before the socket thread can start it;
    /// stop joins that thread before freeing the FFI handle.
    pub fn startCaptured(
        io: std.Io,
        gpa: std.mem.Allocator,
        bridge: *transport.Bridge,
        handle: native_sdk.ChannelHandle,
        endpoint: Endpoint,
        tunnel: remote.Tunnel,
    ) !*Worker {
        if (endpoint != .remote) {
            tunnel.close();
            return error.InvalidCapturedEndpoint;
        }
        return startOwned(io, gpa, bridge, handle, endpoint, .{}, tunnel);
    }

    fn startOwned(
        io: std.Io,
        gpa: std.mem.Allocator,
        bridge: *transport.Bridge,
        handle: native_sdk.ChannelHandle,
        endpoint: Endpoint,
        options: startup.Options,
        tunnel: ?remote.Tunnel,
    ) !*Worker {
        errdefer if (tunnel) |owned| owned.close();
        const worker = try gpa.create(Worker);
        errdefer gpa.destroy(worker);
        worker.* = .{ .gpa = gpa, .io = io, .bridge = bridge, .handle = handle, .endpoint = endpoint, .tunnel = tunnel };
        worker.startup_options = options;
        worker.outgoing_wake_fd = try bridge.outgoing.enableWake();
        errdefer bridge.outgoing.disableWake();
        worker.thread = try std.Thread.spawn(.{}, run, .{worker});
        return worker;
    }

    /// Cancellation and socket shutdown precede join, so a blocked read,
    /// write, connect, or resolver wait cannot outlive the queues. Joining also
    /// guarantees that no channel post can occur after this method returns.
    pub fn stop(worker: *Worker) void {
        worker.stopping.store(true, .release);
        worker.bridge.outgoing.signalWake();
        worker.lockFd();
        if (worker.fd >= 0) _ = std.c.shutdown(worker.fd, std.c.SHUT.RDWR);
        worker.unlockFd();
        if (worker.thread) |thread| thread.join();
        // After the join: the worker's own end is closed, so the tunnel is
        // winding down already, and freeing cancels anything still dialing.
        if (worker.tunnel) |tunnel| tunnel.close();
        worker.bridge.outgoing.disableWake();
        worker.gpa.destroy(worker);
    }

    fn lockFd(worker: *Worker) void {
        while (!worker.fd_mutex.tryLock()) std.atomic.spinLoopHint();
    }

    fn unlockFd(worker: *Worker) void {
        worker.fd_mutex.unlock();
    }

    /// Publishes a newly opened descriptor unless cancellation already won.
    fn publishFd(worker: *Worker, fd: posix.fd_t) bool {
        worker.lockFd();
        defer worker.unlockFd();
        if (worker.stopping.load(.acquire)) return false;
        worker.fd = fd;
        return true;
    }

    fn closeFd(worker: *Worker, fd: posix.fd_t) void {
        worker.lockFd();
        defer worker.unlockFd();
        if (worker.fd == fd) worker.fd = -1;
        _ = std.c.close(fd);
    }

    fn run(worker: *Worker) void {
        worker.ensureCoordinator() catch {
            worker.disconnected(.socket_lost);
            return;
        };
        const fd = connect(worker) catch {
            worker.disconnected(.socket_lost);
            return;
        };
        // Attach first. Optional setup-CLI discovery must never hold the socket
        // pump behind installed executable probes, including during reconnect.
        const discovery = worker.discoverLocal();
        defer if (discovery) |task| task.stop();
        worker.runSocket(fd);
    }

    fn discoverLocal(worker: *Worker) ?*startup.Discovery {
        return switch (worker.endpoint) {
            .unix => |path| startup.Discovery.start(worker.gpa, worker.io, path, worker.startup_options.status) catch null,
            .tcp, .remote => null,
        };
    }

    fn ensureCoordinator(worker: *Worker) !void {
        switch (worker.endpoint) {
            .unix => |path| try worker.ensureLocal(path),
            // A remote coordinator is the remote host's to supervise.
            .tcp, .remote => {},
        }
    }

    fn ensureLocal(worker: *Worker, path: []const u8) !void {
        var evidence: startup.Evidence = .{};
        var options = worker.startup_options;
        options.evidence = &evidence;
        startup.ensure(worker.gpa, worker.io, path, &worker.stopping, options) catch |err| {
            if (options.status) |status| status.record(evidence, path, err);
            return err;
        };
        if (options.status) |status| status.record(evidence, path, null);
    }

    fn runSocket(worker: *Worker, fd: posix.fd_t) void {
        configureSocket(fd) catch {
            worker.closeFd(fd);
            worker.disconnected(.socket_lost);
            return;
        };
        defer {
            worker.closeFd(fd);
            worker.noteRemoteEnd();
            worker.disconnected(.socket_lost);
        }

        while (!worker.stopping.load(.acquire)) {
            worker.bridge.outgoing.drainWake();
            if (!flushOutgoing(worker, fd, null)) return;
            switch (waitReadable(worker, fd, -1) catch return) {
                .retry, .outgoing => continue,
                .readable => if (!worker.receiveFrame(fd)) return,
            }
        }
        worker.bridge.incoming.markDisconnected(.stopped);
    }

    fn receiveFrame(worker: *Worker, fd: posix.fd_t) bool {
        var header: [4]u8 = undefined;
        var frame_started: ?posix.timespec = monotonicTime() orelse return false;
        if (!readExact(worker, fd, &header, &frame_started)) return false;
        const len: usize = std.mem.readInt(u32, &header, .big);
        if (len == 0 or len > transport.max_frame_bytes - header.len) {
            worker.bridge.incoming.markDisconnected(.oversized_frame);
            worker.wake();
            return false;
        }
        const frame = worker.gpa.alloc(u8, header.len + len) catch {
            worker.bridge.incoming.markDisconnected(.queue_overflow);
            worker.wake();
            return false;
        };
        @memcpy(frame[0..header.len], &header);
        if (!readExact(worker, fd, frame[header.len..], &frame_started)) {
            worker.gpa.free(frame);
            return false;
        }
        const staged = worker.bridge.incoming.stageOwned(frame);
        worker.wake();
        return staged;
    }

    fn disconnected(worker: *Worker, reason: transport.DisconnectReason) void {
        if (worker.stopping.load(.acquire)) return;
        worker.bridge.incoming.markDisconnected(reason);
        worker.wake();
    }

    fn wake(worker: *Worker) void {
        if (worker.stopping.load(.acquire)) return;
        _ = worker.handle.post(&transport.wake_payload);
    }

    /// Copy a remote tunnel's failure reason into the provider's record
    /// before the disconnect is posted. The tunnel publishes the reason
    /// before closing its end, so the EOF that brought us here implies it is
    /// already readable.
    fn noteRemoteEnd(worker: *Worker) void {
        const tunnel = worker.tunnel orelse return;
        const status = worker.remoteStatus() orelse return;
        const described = tunnel.describe();
        if (described.state == .failed) status.recordFailure(described.message.slice());
    }

    fn remoteStatus(worker: *const Worker) ?*remote.Status {
        return switch (worker.endpoint) {
            .remote => |endpoint| endpoint.status,
            else => null,
        };
    }
};

const Readiness = enum { retry, readable, outgoing };

/// Outgoing readiness interrupts both idle and partial-frame reads. Only an
/// in-progress frame needs a finite timeout to recheck its existing deadline.
fn waitReadable(worker: *Worker, fd: posix.fd_t, timeout_ms: i32) !Readiness {
    var poll_fds = [_]posix.pollfd{
        .{ .fd = fd, .events = posix.POLL.IN, .revents = 0 },
        .{ .fd = worker.outgoing_wake_fd, .events = posix.POLL.IN, .revents = 0 },
    };
    const timeout = if (worker.bridge.outgoing.hasPending()) 0 else timeout_ms;
    const ready = try posix.poll(&poll_fds, timeout);
    if (ready == 0) return .retry;
    // Prefer readable data over HUP, preserving the peer's final full frame.
    if (poll_fds[0].revents & posix.POLL.IN != 0) return .readable;
    const failures = posix.POLL.ERR | posix.POLL.HUP | posix.POLL.NVAL;
    if ((poll_fds[0].revents | poll_fds[1].revents) & failures != 0)
        return error.SocketLost;
    if (poll_fds[1].revents & posix.POLL.IN != 0) return .outgoing;
    return .retry;
}

fn flushOutgoing(worker: *Worker, fd: posix.fd_t, frame_started: ?posix.timespec) bool {
    for (0..max_flush_batch) |_| {
        const frame = worker.bridge.outgoing.take() orelse break;
        defer worker.bridge.outgoing.release(frame);
        if (frame.len < 5 or frame.len > transport.max_frame_bytes) return false;
        var header: [4]u8 = undefined;
        @memcpy(header[0..], frame[0..4]);
        const declared = std.mem.readInt(u32, &header, .big);
        if (declared == 0 or @as(usize, declared) != frame.len - 4) return false;
        if (!writeExact(worker, fd, frame, frame_started)) return false;
    }
    if (worker.bridge.outgoing.takeDisconnect() != null) return false;
    return true;
}

fn readExact(worker: *Worker, fd: posix.fd_t, output: []u8, frame_started: *?posix.timespec) bool {
    var offset: usize = 0;
    while (offset < output.len) {
        if (frameExpired(frame_started.*)) return false;
        if (worker.stopping.load(.acquire)) return false;
        // Replies are flushed between partial reads so a peer waiting for a
        // response cannot deadlock a large incoming frame.
        worker.bridge.outgoing.drainWake();
        if (!flushOutgoing(worker, fd, frame_started.*)) return false;
        switch (waitReadable(worker, fd, 50) catch return false) {
            .retry, .outgoing => continue,
            .readable => {},
        }
        offset += readFrameChunk(fd, output[offset..], frame_started) orelse return false;
    }
    return true;
}

fn readFrameChunk(fd: posix.fd_t, output: []u8, frame_started: *?posix.timespec) ?usize {
    const count = posix.read(fd, output) catch return null;
    if (count == 0) return null;
    if (frame_started.* == null) frame_started.* = monotonicTime() orelse return null;
    return count;
}

fn frameExpired(started: ?posix.timespec) bool {
    const value = started orelse return false;
    const now = monotonicTime() orelse return true;
    return elapsedNanos(value, now) >= 5 * std.time.ns_per_s;
}

/// Send `input` in full within one second, or give up and report failure.
///
/// The one-second budget is this function's contract with its callers: a peer
/// that has stopped reading must not be able to park the extension worker on a
/// single frame. That budget can only be evaluated BETWEEN send() calls, so the
/// contract holds only if send() itself never blocks.
///
/// MSG.DONTWAIT does not buy that on macOS. Darwin ignores MSG_DONTWAIT on an
/// AF_UNIX socket that is blocking at the descriptor level; only O_NONBLOCK
/// makes the kernel return EAGAIN. Because the flag is present as a decl, the
/// comptime @hasDecl guard below compiles happily and the code reads as though
/// it were non-blocking while the syscall sleeps indefinitely. That is exactly
/// how the deadline came to be unreachable.
///
/// Measured by scripts/measure-send-blocking.c (SNDBUF 4096, 16 MiB payload,
/// peer never reads) on macOS 25.5 / arm64:
///
///   blocking fd + MSG_DONTWAIT   still inside send() when a 5s alarm fired
///   O_NONBLOCK fd                EAGAIN after 0.000s, 4096 bytes written
///   blocking fd + SO_SNDTIMEO=1s EAGAIN after 2.004s
///
/// So SO_SNDTIMEO is rejected as the fix: Darwin overshoots it by a consistent
/// factor of two (250ms -> 0.503s, 500ms -> 1.004s, 1s -> 2.004s, 2s -> 4.004s),
/// which would quietly turn this one-second budget into a two-second one.
/// O_NONBLOCK is exact, so the descriptor is switched for the duration of the
/// write and put back the way it was found.
///
/// Restoring matters as much as setting. connectAddress deliberately hands back
/// a BLOCKING descriptor, and readExact treats any read error as a dead
/// connection -- so leaking O_NONBLOCK out of this function would let a benign
/// EAGAIN on the read side tear down a healthy session.
fn writeExact(worker: ?*Worker, fd: posix.fd_t, input: []const u8, frame_started: ?posix.timespec) bool {
    const was_nonblocking = isNonblocking(fd) catch return false;
    if (!was_nonblocking) setNonblocking(fd, true) catch return false;
    defer if (!was_nonblocking) {
        setNonblocking(fd, false) catch {};
    };

    var offset: usize = 0;
    const started = monotonicTime() orelse return false;
    while (offset < input.len) {
        const now = monotonicTime() orelse return false;
        const elapsed_ns = elapsedNanos(started, now);
        if (elapsed_ns >= std.time.ns_per_s) return false;
        if (frame_started) |value| {
            if (elapsedNanos(value, now) >= 5 * std.time.ns_per_s) return false;
        }
        if (worker) |value| {
            if (value.stopping.load(.acquire)) return false;
        }
        const flags: u32 = (if (comptime @hasDecl(posix.MSG, "NOSIGNAL")) posix.MSG.NOSIGNAL else 0) |
            (if (comptime @hasDecl(posix.MSG, "DONTWAIT")) posix.MSG.DONTWAIT else 0);
        const rc = std.c.send(fd, input[offset..].ptr, input.len - offset, flags);
        switch (posix.errno(rc)) {
            .SUCCESS => {
                if (rc == 0) return false;
                offset += @intCast(rc);
            },
            .INTR => continue,
            .AGAIN => {
                var poll_fds = [_]posix.pollfd{.{
                    .fd = fd,
                    .events = posix.POLL.OUT,
                    .revents = 0,
                }};
                const ready = posix.poll(&poll_fds, 50) catch return false;
                if (ready == 0) continue;
                if (poll_fds[0].revents & posix.POLL.OUT == 0) return false;
            },
            else => return false,
        }
    }
    return true;
}

fn monotonicTime() ?posix.timespec {
    var value: posix.timespec = undefined;
    return switch (posix.errno(posix.system.clock_gettime(.MONOTONIC, &value))) {
        .SUCCESS => value,
        else => null,
    };
}

fn elapsedNanos(started: posix.timespec, now: posix.timespec) i128 {
    return (@as(i128, now.sec) - @as(i128, started.sec)) * std.time.ns_per_s +
        (@as(i128, now.nsec) - @as(i128, started.nsec));
}

fn configureSocket(fd: posix.fd_t) !void {
    // Linux supports per-send MSG_NOSIGNAL; Darwin uses SO_NOSIGPIPE. Applying
    // both conditionally avoids changing process-global SIGPIPE disposition.
    if (comptime @hasDecl(posix.SO, "NOSIGPIPE")) {
        const enabled: c_int = 1;
        try posix.setsockopt(fd, posix.SOL.SOCKET, posix.SO.NOSIGPIPE, std.mem.asBytes(&enabled));
    }
}

fn connect(worker: *Worker) !posix.fd_t {
    return switch (worker.endpoint) {
        .tcp => |tcp| connectTcp(worker, tcp.host, tcp.port),
        .unix => |path| connectUnix(worker, path),
        .remote => |endpoint| connectRemote(worker, endpoint),
    };
}

/// A registered remote host is a Unix-domain socket pair whose far end
/// belongs to a phux-client-ffi tunnel. Everything after this returns is the
/// ordinary framed worker: the tunnel relays whole frames, so nothing below
/// can tell a local coordinator from a remote one.
fn connectRemote(worker: *Worker, endpoint: Endpoint.Remote) !posix.fd_t {
    if (worker.stopping.load(.acquire)) return error.Canceled;
    const tunnel = worker.tunnel orelse try resolveRemote(endpoint);
    // From here stop owns the tunnel, including failed resolution/start paths.
    worker.tunnel = tunnel;
    return connectTunnel(worker, tunnel, endpoint.status);
}

fn resolveRemote(endpoint: Endpoint.Remote) !remote.Tunnel {
    return remote.Tunnel.resolve(endpoint.target, endpoint.config_path) catch |err| {
        if (endpoint.status) |status| status.recordFailure("that is not a host name Cockpit can look up");
        return err;
    };
}

fn connectTunnel(worker: *Worker, tunnel: remote.Tunnel, status: ?*remote.Status) !posix.fd_t {
    const described = tunnel.describe();
    if (described.state != .resolved) {
        if (status) |record| record.recordFailure(described.message.slice());
        return error.RemoteUnresolved;
    }
    const pair = try remoteSocketPair();
    if (!worker.publishFd(pair[0])) {
        for (pair) |fd| _ = std.c.close(fd);
        return error.Canceled;
    }
    errdefer worker.closeFd(pair[0]);
    // Ownership of pair[1] transfers to the tunnel on every path.
    tunnel.start(pair[1]) catch |err| {
        worker.noteRemoteEnd();
        return err;
    };
    return pair[0];
}

/// Both ends close-on-exec (a coordinator helper must never inherit a live
/// remote connection) and SIGPIPE-free: the tunnel writes its end from a
/// library thread and cannot change this process's signal disposition.
fn remoteSocketPair() ![2]posix.fd_t {
    var pair: [2]posix.fd_t = undefined;
    if (std.c.socketpair(@intCast(posix.AF.UNIX), @intCast(posix.SOCK.STREAM), 0, &pair) != 0)
        return error.SocketPairFailed;
    errdefer for (pair) |fd| {
        _ = std.c.close(fd);
    };
    for (pair) |fd| {
        if (std.c.fcntl(fd, posix.F.SETFD, @as(c_int, posix.FD_CLOEXEC)) < 0) return error.SocketPairFailed;
        try configureSocket(fd);
    }
    return pair;
}

fn connectTcp(worker: *Worker, host: []const u8, port: u16) !posix.fd_t {
    if (port == 0) return error.InvalidPort;
    if (std.Io.net.IpAddress.parse(host, port)) |address| {
        return connectIpAddress(worker, address);
    } else |_| {}

    const addresses = try resolveHost(worker, host, port);
    var last_error: ?anyerror = null;
    for (addresses.items[0..addresses.len]) |address| {
        return connectIpAddress(worker, address) catch |err| {
            if (worker.stopping.load(.acquire)) return error.Canceled;
            last_error = err;
            continue;
        };
    }
    return last_error orelse error.UnknownHostName;
}

fn connectIpAddress(worker: *Worker, address: std.Io.net.IpAddress) !posix.fd_t {
    return switch (address) {
        .ip4 => |ip4| {
            var socket_address: posix.sockaddr.in = .{
                .port = std.mem.nativeToBig(u16, ip4.port),
                .addr = @bitCast(ip4.bytes),
            };
            return connectAddress(
                worker,
                @ptrCast(&socket_address),
                @sizeOf(@TypeOf(socket_address)),
                posix.AF.INET,
            );
        },
        .ip6 => |ip6| {
            var socket_address: posix.sockaddr.in6 = .{
                .port = std.mem.nativeToBig(u16, ip6.port),
                .flowinfo = ip6.flow,
                .addr = ip6.bytes,
                .scope_id = ip6.interface.index,
            };
            return connectAddress(
                worker,
                @ptrCast(&socket_address),
                @sizeOf(@TypeOf(socket_address)),
                posix.AF.INET6,
            );
        },
    };
}

fn resolveHost(worker: *Worker, host: []const u8, port: u16) !ResolvedAddresses {
    _ = try std.Io.net.HostName.init(host);
    const context = try std.heap.page_allocator.create(ResolveContext);
    context.* = .{ .host_len = host.len, .port = port };
    @memcpy(context.host[0..host.len], host);
    const thread = std.Thread.spawn(.{}, ResolveContext.run, .{context}) catch |err| {
        std.heap.page_allocator.destroy(context);
        return err;
    };
    thread.detach();

    while (true) {
        if (context.state.load(.acquire) == 1) {
            const result = context.result;
            std.heap.page_allocator.destroy(context);
            if (result.len == 0) return error.UnknownHostName;
            return result;
        }
        if (worker.stopping.load(.acquire)) {
            const previous = context.state.cmpxchgStrong(0, 2, .release, .acquire);
            if (previous == 1) std.heap.page_allocator.destroy(context);
            return error.Canceled;
        }
        std.Io.sleep(worker.io, std.Io.Duration.fromMilliseconds(10), .awake) catch {};
    }
}

fn resolveBlocking(host: [:0]const u8, port: u16) ResolvedAddresses {
    var output: ResolvedAddresses = .{};
    var port_buffer: [8]u8 = undefined;
    const port_z = std.fmt.bufPrintZ(&port_buffer, "{d}", .{port}) catch return output;
    const hints: posix.addrinfo = .{
        .flags = .{ .NUMERICSERV = true },
        .family = posix.AF.UNSPEC,
        .socktype = posix.SOCK.STREAM,
        .protocol = posix.IPPROTO.TCP,
        .canonname = null,
        .addr = null,
        .addrlen = 0,
        .next = null,
    };
    var result: ?*posix.addrinfo = null;
    if (posix.system.getaddrinfo(host.ptr, port_z.ptr, &hints, &result) != @as(posix.system.EAI, @enumFromInt(0)))
        return output;
    defer if (result) |head| posix.system.freeaddrinfo(head);
    var cursor = result;
    while (cursor) |info| : (cursor = info.next) {
        if (output.len == output.items.len) break;
        const address = info.addr orelse continue;
        if (address.family != posix.AF.INET and address.family != posix.AF.INET6) continue;
        const wrapped: *const std.Io.Threaded.PosixAddress = @alignCast(@fieldParentPtr("any", address));
        output.items[output.len] = std.Io.Threaded.addressFromPosix(wrapped);
        output.len += 1;
    }
    return output;
}

fn connectUnix(worker: *Worker, path: []const u8) !posix.fd_t {
    if (path.len == 0 or path.len >= @sizeOf(@FieldType(posix.sockaddr.un, "path"))) return error.InvalidUnixPath;
    var socket_address = std.mem.zeroes(posix.sockaddr.un);
    socket_address.len = @intCast(@offsetOf(posix.sockaddr.un, "path") + path.len + 1);
    socket_address.family = posix.AF.UNIX;
    @memcpy(socket_address.path[0..path.len], path);
    return connectAddress(
        worker,
        @ptrCast(&socket_address),
        socket_address.len,
        posix.AF.UNIX,
    );
}

fn connectAddress(
    worker: *Worker,
    address: *const posix.sockaddr,
    address_len: posix.socklen_t,
    family: posix.sa_family_t,
) !posix.fd_t {
    if (worker.stopping.load(.acquire)) return error.Canceled;
    const fd = std.c.socket(@intCast(family), posix.SOCK.STREAM, 0);
    if (fd < 0) return error.SocketOpenFailed;
    if (!worker.publishFd(fd)) {
        _ = std.c.close(fd);
        return error.Canceled;
    }
    errdefer worker.closeFd(fd);

    try setNonblocking(fd, true);
    const rc = std.c.connect(fd, address, address_len);
    if (rc != 0 and posix.errno(rc) != .INPROGRESS) return error.ConnectFailed;
    if (rc != 0) try waitConnected(fd, &worker.stopping);
    try setNonblocking(fd, false);
    return fd;
}

pub fn waitConnected(fd: posix.fd_t, stopping: *const std.atomic.Value(bool)) !void {
    while (!stopping.load(.acquire)) {
        var poll_fds = [_]posix.pollfd{.{
            .fd = fd,
            .events = posix.POLL.OUT,
            .revents = 0,
        }};
        const ready = try posix.poll(&poll_fds, 50);
        if (ready == 0) continue;
        var socket_error: c_int = 0;
        var error_len: posix.socklen_t = @sizeOf(c_int);
        if (std.c.getsockopt(fd, posix.SOL.SOCKET, posix.SO.ERROR, &socket_error, &error_len) != 0)
            return error.ConnectFailed;
        if (socket_error != 0) return error.ConnectFailed;
        return;
    }
    return error.Canceled;
}

/// Read back what setNonblocking would set, so writeExact can leave a
/// descriptor exactly as it found it rather than assuming it arrived blocking.
fn isNonblocking(fd: posix.fd_t) !bool {
    const raw_flags = std.c.fcntl(fd, posix.F.GETFL);
    if (raw_flags < 0) return error.FcntlFailed;
    const flags: posix.O = @bitCast(@as(u32, @intCast(raw_flags)));
    return flags.NONBLOCK;
}

pub fn setNonblocking(fd: posix.fd_t, enabled: bool) !void {
    const raw_flags = std.c.fcntl(fd, posix.F.GETFL);
    if (raw_flags < 0) return error.FcntlFailed;
    var flags: posix.O = @bitCast(@as(u32, @intCast(raw_flags)));
    flags.NONBLOCK = enabled;
    if (std.c.fcntl(fd, posix.F.SETFL, @as(c_int, @bitCast(flags))) < 0) return error.FcntlFailed;
}

fn socketPair() ![2]posix.fd_t {
    if (comptime builtin.os.tag == .windows) return error.SkipZigTest;
    var sockets: [2]posix.fd_t = undefined;
    if (std.c.socketpair(@intCast(posix.AF.UNIX), @intCast(posix.SOCK.STREAM), 0, &sockets) != 0)
        return error.SocketPairFailed;
    return sockets;
}

test "write after peer teardown reports failure without process signal" {
    const sockets = try socketPair();
    defer _ = std.c.close(sockets[0]);
    try configureSocket(sockets[0]);
    _ = std.c.close(sockets[1]);
    try std.testing.expect(!writeExact(null, sockets[0], "terminal reply", null));
}

test "a non-reading peer cannot hold one frame writer forever" {
    const sockets = try socketPair();
    defer _ = std.c.close(sockets[0]);
    defer _ = std.c.close(sockets[1]);
    const small_buffer: c_int = 4096;
    try posix.setsockopt(sockets[0], posix.SOL.SOCKET, posix.SO.SNDBUF, std.mem.asBytes(&small_buffer));
    const payload = try std.testing.allocator.alloc(u8, 16 * 1024 * 1024);
    defer std.testing.allocator.free(payload);
    @memset(payload, 0x5a);

    // Nobody ever reads sockets[1], so this write can never complete. Until
    // writeExact forced O_NONBLOCK, this call sat inside send() indefinitely
    // and took the whole test binary with it: rooting extension.zig reported
    // "5 pass, 1 crash" only after the run was killed at 180 seconds.
    const started = monotonicTime() orelse return error.SkipZigTest;
    try std.testing.expect(!writeExact(null, sockets[0], payload, null));
    const elapsed_ns = elapsedNanos(started, monotonicTime() orelse return error.SkipZigTest);

    // Both bounds carry weight. The upper bound is the invariant this test is
    // named for -- the budget is one second plus at most one 50ms poll, so
    // crossing three seconds means the deadline has become unreachable again.
    // The lower bound guards the opposite regression: a writeExact that treated
    // EAGAIN as fatal would return false immediately, satisfy the assertion
    // above, and silently fail frames that were merely waiting for buffer.
    try std.testing.expect(elapsed_ns >= 900 * std.time.ns_per_ms);
    try std.testing.expect(elapsed_ns < 3 * std.time.ns_per_s);
}

test "writeExact restores the blocking mode it was handed" {
    const sockets = try socketPair();
    defer _ = std.c.close(sockets[0]);
    defer _ = std.c.close(sockets[1]);

    // connectAddress deliberately hands framed IO a BLOCKING descriptor, and
    // readExact treats any read error as a dead connection. O_NONBLOCK leaking
    // out of writeExact would therefore let a benign EAGAIN on the read side
    // tear down a perfectly healthy session.
    try std.testing.expectEqual(false, try isNonblocking(sockets[0]));
    try std.testing.expect(writeExact(null, sockets[0], "frame", null));
    try std.testing.expectEqual(false, try isNonblocking(sockets[0]));

    // A descriptor that arrives non-blocking has to stay non-blocking; the
    // restore must put back what was there, not a fixed assumption.
    try setNonblocking(sockets[0], true);
    try std.testing.expect(writeExact(null, sockets[0], "frame", null));
    try std.testing.expectEqual(true, try isNonblocking(sockets[0]));
}

test "final complete frame remains readable when peer has shut down" {
    const sockets = try socketPair();
    defer _ = std.c.close(sockets[0]);
    defer _ = std.c.close(sockets[1]);
    const frame = [_]u8{ 0, 0, 0, 1, 0x42 };
    try std.testing.expect(writeExact(null, sockets[0], &frame, null));
    try std.testing.expectEqual(@as(c_int, 0), std.c.shutdown(sockets[0], std.c.SHUT.WR));

    var poll_fds = [_]posix.pollfd{.{
        .fd = sockets[1],
        .events = posix.POLL.IN,
        .revents = 0,
    }};
    try std.testing.expectEqual(@as(usize, 1), try posix.poll(&poll_fds, 1000));
    try std.testing.expect(poll_fds[0].revents & posix.POLL.IN != 0);

    var received: [frame.len]u8 = undefined;
    var offset: usize = 0;
    while (offset < received.len) {
        const count = try posix.read(sockets[1], received[offset..]);
        try std.testing.expect(count > 0);
        offset += count;
    }
    try std.testing.expectEqualSlices(u8, &frame, &received);
}

test "connect wait observes cancellation without a blocking syscall" {
    const sockets = try socketPair();
    defer _ = std.c.close(sockets[0]);
    defer _ = std.c.close(sockets[1]);
    const stopping = std.atomic.Value(bool).init(true);
    try std.testing.expectError(error.Canceled, waitConnected(sockets[0], &stopping));
}

test "nonblocking connect mode can be restored for framed IO" {
    const sockets = try socketPair();
    defer _ = std.c.close(sockets[0]);
    defer _ = std.c.close(sockets[1]);
    try setNonblocking(sockets[0], true);
    try setNonblocking(sockets[0], false);
    try std.testing.expect(writeExact(null, sockets[0], "frame", null));
}

test "localhost resolves without requiring a numeric TCP address" {
    const addresses = resolveBlocking("localhost", 4321);
    try std.testing.expect(addresses.len > 0);
    for (addresses.items[0..addresses.len]) |address|
        try std.testing.expectEqual(@as(u16, 4321), address.getPort());
}

test "staging after the idle flush makes socket wait outgoing-ready" {
    const sockets = try socketPair();
    defer _ = std.c.close(sockets[0]);
    defer _ = std.c.close(sockets[1]);
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    var worker: Worker = .{
        .gpa = std.testing.allocator,
        .io = std.testing.io,
        .bridge = &bridge,
        .handle = .{},
        .endpoint = .{ .unix = "unused" },
        .outgoing_wake_fd = try bridge.outgoing.enableWake(),
    };
    bridge.outgoing.drainWake();
    try std.testing.expect(flushOutgoing(&worker, sockets[0], null));
    try std.testing.expectEqual(Readiness.retry, try waitReadable(&worker, sockets[0], 0));

    // Exactly the flush-to-poll race window, with a silent peer. A zero-time
    // poll proves readiness without relying on scheduling or a latency bound.
    const frame = "\x00\x00\x00\x01k";
    try std.testing.expect(bridge.outgoing.stage(frame));
    try std.testing.expectEqual(Readiness.outgoing, try waitReadable(&worker, sockets[0], 0));
    bridge.outgoing.drainWake();
    try std.testing.expect(flushOutgoing(&worker, sockets[0], null));
    var received: [frame.len]u8 = undefined;
    try std.testing.expectEqual(frame.len, try posix.read(sockets[1], &received));
    try std.testing.expectEqualSlices(u8, frame, &received);
    try std.testing.expectEqual(Readiness.retry, try waitReadable(&worker, sockets[0], 0));

    // Queue failure must interrupt idle IO too, even when no frame was added.
    bridge.outgoing.markDisconnected(.queue_overflow);
    try std.testing.expectEqual(Readiness.outgoing, try waitReadable(&worker, sockets[0], 0));
    bridge.outgoing.drainWake();
    try std.testing.expect(!flushOutgoing(&worker, sockets[0], null));
}

/// Bypass only address resolution/connect: exercise the production connected
/// worker, its real poll loop, and Worker.stop over a real Unix socket pair.
fn startSocketWorker(bridge: *transport.Bridge, fd: posix.fd_t) !*Worker {
    const gpa = std.testing.allocator;
    const worker = try gpa.create(Worker);
    errdefer gpa.destroy(worker);
    worker.* = .{
        .gpa = gpa,
        .io = std.testing.io,
        .bridge = bridge,
        .handle = .{},
        .endpoint = .{ .unix = "unused" },
        .fd = fd,
        .outgoing_wake_fd = try bridge.outgoing.enableWake(),
    };
    errdefer bridge.outgoing.disableWake();
    worker.thread = try std.Thread.spawn(.{}, Worker.runSocket, .{ worker, fd });
    return worker;
}

fn receiveTestFrame(fd: posix.fd_t, expected: []const u8) !void {
    var received: [64]u8 = undefined;
    var offset: usize = 0;
    while (offset < expected.len) {
        var polls = [_]posix.pollfd{.{ .fd = fd, .events = posix.POLL.IN, .revents = 0 }};
        try std.testing.expectEqual(@as(usize, 1), try posix.poll(&polls, 1000));
        const count = try posix.read(fd, received[offset..expected.len]);
        try std.testing.expect(count > 0);
        offset += count;
    }
    try std.testing.expectEqualSlices(u8, expected, received[0..expected.len]);
}

test "idle socket worker sends staged keys and stops with no peer traffic" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const sockets = try socketPair();
    defer _ = std.c.close(sockets[1]);
    const worker = startSocketWorker(&bridge, sockets[0]) catch |err| {
        _ = std.c.close(sockets[0]);
        return err;
    };
    var stopped = false;
    defer if (!stopped) worker.stop();

    const frame = "\x00\x00\x00\x01k";
    var samples: [32]i128 = undefined;
    for (&samples) |*sample| {
        // Measurement-only dwell lets the real worker park. Correctness is
        // asserted by the zero-time readiness guard, not a tight timing limit.
        try std.Io.sleep(std.testing.io, .fromMilliseconds(5), .awake);
        const started = monotonicTime().?;
        try std.testing.expect(bridge.outgoing.stage(frame));
        try receiveTestFrame(sockets[1], frame);
        sample.* = elapsedNanos(started, monotonicTime().?);
    }
    std.mem.sort(i128, &samples, {}, std.sort.asc(i128));
    std.debug.print("idle socket key staging-to-peer: n=32 p50={d}us p99={d}us\n", .{
        @divTrunc(samples[16], 1000), @divTrunc(samples[31], 1000),
    });
    try std.Io.sleep(std.testing.io, .fromMilliseconds(5), .awake);
    const started = monotonicTime().?;
    worker.stop();
    stopped = true;
    try std.testing.expect(elapsedNanos(started, monotonicTime().?) < std.time.ns_per_s);
    // Detached staging must not touch closed descriptors; a replacement worker
    // receives this queued frame through its own freshly attached wake pipe.
    try std.testing.expect(bridge.outgoing.stage(frame));
    const replacement = try socketPair();
    defer _ = std.c.close(replacement[1]);
    const next = startSocketWorker(&bridge, replacement[0]) catch |err| {
        _ = std.c.close(replacement[0]);
        return err;
    };
    defer next.stop();
    try receiveTestFrame(replacement[1], frame);
}

test "partial inbound frame does not hold outgoing bursts behind its read" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const sockets = try socketPair();
    defer _ = std.c.close(sockets[1]);
    const frame = "\x00\x00\x00\x01k";
    // Exceed a flush batch under one coalesced token, and leave the peer's
    // incoming frame incomplete while the worker must continue sending.
    for (0..2 * max_flush_batch) |_| try std.testing.expect(bridge.outgoing.stage(frame));
    try std.testing.expect(writeExact(null, sockets[1], frame[0..4], null));
    const worker = startSocketWorker(&bridge, sockets[0]) catch |err| {
        _ = std.c.close(sockets[0]);
        return err;
    };
    defer worker.stop();
    for (0..2 * max_flush_batch) |_| try receiveTestFrame(sockets[1], frame);
    try std.Io.sleep(std.testing.io, .fromMilliseconds(5), .awake);
    try std.testing.expect(bridge.outgoing.stage(frame));
    try receiveTestFrame(sockets[1], frame);
    try std.testing.expect(writeExact(null, sockets[1], frame[4..], null));

    const started = monotonicTime().?;
    while (!bridge.incoming.hasPending()) {
        try std.testing.expect(elapsedNanos(started, monotonicTime().?) < std.time.ns_per_s);
        try std.Io.sleep(std.testing.io, .fromMilliseconds(1), .awake);
    }
    const incoming = bridge.incoming.take().?;
    defer bridge.incoming.release(incoming);
    try std.testing.expectEqualSlices(u8, frame, incoming);
}

test "Unix worker ensures selected coordinator before attempting its socket" {
    var fixture = try startup.TestFixture.init();
    defer fixture.deinit();
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const worker = try Worker.startWithOptions(std.testing.io, std.testing.allocator, &bridge, .{}, .{ .unix = fixture.socket }, .{ .cli_path = fixture.cli });
    defer worker.stop();
    const started = monotonicTime().?;
    var reason: ?transport.DisconnectReason = null;
    while (!fixture.ready() and reason == null) {
        try std.testing.expect(elapsedNanos(started, monotonicTime().?) < 5 * std.time.ns_per_s);
        try std.Io.sleep(std.testing.io, .fromMilliseconds(1), .awake);
        reason = bridge.incoming.takeDisconnect();
    }
    // The selected socket does not exist. Connecting before ensure finishes
    // would already have posted a disconnect; the live worker must still wait.
    try std.testing.expect(fixture.ready());
    try std.testing.expectEqual(null, reason);
    try std.testing.expectEqual(null, bridge.incoming.takeDisconnect());
    try fixture.checkArguments();
    try fixture.release("0");
    while (reason == null) {
        try std.testing.expect(elapsedNanos(started, monotonicTime().?) < 5 * std.time.ns_per_s);
        try std.Io.sleep(std.testing.io, .fromMilliseconds(1), .awake);
        reason = bridge.incoming.takeDisconnect();
    }
    try std.testing.expectEqual(transport.DisconnectReason.socket_lost, reason.?);
    try fixture.expectExitCode("0");
}

extern "c" fn setenv([*:0]const u8, [*:0]const u8, c_int) c_int;
extern "c" fn unsetenv([*:0]const u8) c_int;

test "running coordinator socket pump starts before optional slow CLI discovery" {
    var fixture = try startup.TestFixture.init();
    defer fixture.deinit();
    const previous = if (std.c.getenv("PHUX_CLI")) |value| try std.testing.allocator.dupeZ(u8, std.mem.span(value)) else null;
    defer {
        if (previous) |value| {
            _ = setenv("PHUX_CLI", value, 1);
            std.testing.allocator.free(value);
        } else _ = unsetenv("PHUX_CLI");
    }
    const cli_z = try std.testing.allocator.dupeZ(u8, fixture.cli);
    defer std.testing.allocator.free(cli_z);
    try std.testing.expectEqual(@as(c_int, 0), setenv("PHUX_CLI", cli_z, 1));
    const listener = try listenTestUnix(fixture.socket);
    defer _ = std.c.close(listener);
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    var status: startup.Status = .{};
    const worker = try Worker.startWithOptions(std.testing.io, std.testing.allocator, &bridge, .{}, .{ .unix = fixture.socket }, .{ .status = &status });
    var stopped = false;
    defer if (!stopped) worker.stop();

    // The availability probe is not the transport connection. Accept and discard
    // it first, then require the real socket pump while the CLI is still blocked.
    var polls = [_]posix.pollfd{.{ .fd = listener, .events = posix.POLL.IN, .revents = 0 }};
    try std.testing.expectEqual(@as(usize, 1), try posix.poll(&polls, 5000));
    const probe = std.c.accept(listener, null, null);
    try std.testing.expect(probe >= 0);
    _ = std.c.close(probe);
    const started = monotonicTime().?;
    while (!fixture.ready()) {
        try std.testing.expect(elapsedNanos(started, monotonicTime().?) < 5 * std.time.ns_per_s);
        try std.Io.sleep(std.testing.io, .fromMilliseconds(1), .awake);
    }
    try std.testing.expectEqual(@as(usize, 1), try posix.poll(&polls, 0));
    const connected = std.c.accept(listener, null, null);
    try std.testing.expect(connected >= 0);
    defer _ = std.c.close(connected);
    const frame = "\x00\x00\x00\x01k";
    try std.testing.expect(bridge.outgoing.stage(frame));
    try receiveTestFrame(connected, frame);
    const stopping_started = monotonicTime().?;
    worker.stop();
    stopped = true;
    try std.testing.expect(elapsedNanos(stopping_started, monotonicTime().?) < std.time.ns_per_s);
    try fixture.expectReaped();
}

fn listenTestUnix(path: []const u8) !posix.fd_t {
    var address = std.mem.zeroes(posix.sockaddr.un);
    if (path.len >= address.path.len) return error.TestSocketTooLong;
    address.len = @intCast(@offsetOf(posix.sockaddr.un, "path") + path.len + 1);
    address.family = posix.AF.UNIX;
    @memcpy(address.path[0..path.len], path);
    const fd = std.c.socket(posix.AF.UNIX, posix.SOCK.STREAM, 0);
    if (fd < 0) return error.TestSocketFailed;
    errdefer _ = std.c.close(fd);
    if (std.c.bind(fd, @ptrCast(&address), address.len) != 0) return error.TestBindFailed;
    if (std.c.listen(fd, 8) != 0) return error.TestListenFailed;
    return fd;
}

test "stopping during coordinator ensure cancels and reaps helper without posting" {
    var fixture = try startup.TestFixture.init();
    defer fixture.deinit();
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const worker = try Worker.startWithOptions(std.testing.io, std.testing.allocator, &bridge, .{}, .{ .unix = fixture.socket }, .{ .cli_path = fixture.cli });
    var stopped = false;
    defer if (!stopped) worker.stop();
    const started = monotonicTime().?;
    while (!fixture.ready()) {
        try std.testing.expect(elapsedNanos(started, monotonicTime().?) < 5 * std.time.ns_per_s);
        try std.Io.sleep(std.testing.io, .fromMilliseconds(1), .awake);
    }
    const stopping_started = monotonicTime().?;
    worker.stop();
    stopped = true;
    try std.testing.expect(elapsedNanos(stopping_started, monotonicTime().?) < std.time.ns_per_s);
    try std.testing.expect(!bridge.incoming.hasPending());
    try std.testing.expectEqual(null, bridge.incoming.takeDisconnect());
    try fixture.checkArguments();
    try fixture.expectReaped();
}

fn awaitDisconnect(bridge: *transport.Bridge) !transport.DisconnectReason {
    const started = monotonicTime().?;
    while (true) {
        if (bridge.incoming.takeDisconnect()) |reason| return reason;
        try std.testing.expect(elapsedNanos(started, monotonicTime().?) < 10 * std.time.ns_per_s);
        try std.Io.sleep(std.testing.io, .fromMilliseconds(5), .awake);
    }
}

test "an unregistered remote host disconnects with the registry's reason and never ensures a coordinator" {
    var registry = try remote.TestRegistry.init("mini", "ws://127.0.0.1:1");
    defer registry.deinit();
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    var status: remote.Status = .{};
    // A CLI path that cannot run: a remote endpoint must never reach ensure.
    const worker = try Worker.startWithOptions(std.testing.io, std.testing.allocator, &bridge, .{}, .{
        .remote = .{ .target = "me@studio", .config_path = registry.path, .status = &status },
    }, .{ .cli_path = "/does-not-exist/phux" });
    defer worker.stop();
    try std.testing.expectEqual(transport.DisconnectReason.socket_lost, try awaitDisconnect(&bridge));
    var reason: [remote.max_text_bytes]u8 = undefined;
    const text = status.failureInto(&reason);
    try std.testing.expect(std.mem.indexOf(u8, text, "phux --remote me@studio") != null);
}

test "a registered host that does not answer records why before the worker reports the loss" {
    // Port 1 on loopback refuses immediately; the tunnel publishes FAILED and
    // its reason, then closes its end, and only then does the worker see EOF.
    var registry = try remote.TestRegistry.init("gone", "ws://127.0.0.1:1");
    defer registry.deinit();
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    var status: remote.Status = .{};
    const worker = try Worker.start(std.testing.io, std.testing.allocator, &bridge, .{}, .{
        .remote = .{ .target = "gone", .config_path = registry.path, .status = &status },
    });
    defer worker.stop();
    try std.testing.expectEqual(transport.DisconnectReason.socket_lost, try awaitDisconnect(&bridge));
    var reason: [remote.max_text_bytes]u8 = undefined;
    const text = status.failureInto(&reason);
    try std.testing.expect(std.mem.indexOf(u8, text, "did not answer") != null);
}
