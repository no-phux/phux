//! AgentSession rows: who is running under which terminal, and what its own
//! record stream says it is doing.
//!
//! An AgentSession is a resource with no replica. The server never publishes a
//! grid for one and the terminal facet refuses it (`PhuxResourceInfo`), so
//! nothing in this file produces a `Presentation` and nothing downstream may
//! turn a session into a surface. What it produces is a ROW under the terminal
//! that owns it, and one attention signal for that terminal.
//!
//! Two inputs, with a deliberate precedence between them. The resource catalog
//! is the MEMBERSHIP authority: a session exists exactly while the latest
//! ATTACHED snapshot lists it, and the `state` span it carries is the server's
//! own published answer. The record stream is the STATE authority: it is the
//! agent describing itself over a sequenced channel it owns, which is the top
//! of the evidence ladder in ADR-0103 decision 5. So a session adopted from
//! the catalog starts at the catalog's word and moves to the stream's the
//! moment one arrives, and a stream for a resource the catalog never listed
//! changes nothing — a row with no parent has nowhere to hang.

const std = @import("std");
const provider = @import("provider_contract");

const RemoteId = provider.RemoteResourceId;

/// Agent roster ceiling, using Cockpit's catalog-row budget independently of
/// terminal discovery. Live replicas are a separate, much smaller budget that
/// agent sessions never draw on.
pub const max_sessions: usize = provider.workspace.max_terminals;

/// Byte ceiling for one `provider` or `native_id` span. Well under the
/// workspace text bound: these are slugs (`claude`, `codex`) and producer-side
/// identifiers, not titles, and the roster keeps one copy of each per session.
pub const max_text_bytes: usize = 256;

/// Presentation budget: four identity spans, independent of the wire payload.
pub const max_reason_bytes: usize = 4 * max_text_bytes;

pub const Reason = struct {
    bytes: [max_reason_bytes]u8 = @splat(0),
    len: usize = 0,
    truncated: bool = false,

    pub fn slice(reason: *const Reason) []const u8 {
        return reason.bytes[0..reason.len];
    }

    fn init(value: []const u8) Error!Reason {
        if (!std.unicode.utf8ValidateSlice(value)) return error.Protocol;
        var result: Reason = .{};
        result.truncated = value.len > max_reason_bytes;
        var end = @min(value.len, max_reason_bytes);
        if (result.truncated) {
            end = max_reason_bytes - 3;
            // Back up over continuation bytes, never splitting a codepoint.
            while (value[end] & 0xc0 == 0x80) end -= 1;
        }
        @memcpy(result.bytes[0..end], value[0..end]);
        result.len = end;
        if (result.truncated) {
            @memcpy(result.bytes[end..][0..3], "...");
            result.len += 3;
        }
        return result;
    }
};

/// Only state-bearing evidence is retained. Text owns its bytes; record_type
/// points at this module's static vocabulary, never at the borrowed JSON.
pub const LatestEvidence = struct {
    seq: ?u64 = null,
    ts_ms: ?u64 = null,
    record_type: []const u8,
    reason: Reason = .{},
};

/// Derived lifecycle of one agent session (ADR-0103 decision 5).
///
/// OPEN decode: a catalog word this vocabulary does not name reads as
/// `unknown`, never as a guess. `gone` is local to this projection — the
/// server retires the resource rather than publishing a word for it — and it
/// exists so a `session_end` seen on the stream stops the row claiming work
/// before the catalog refresh that drops it.
pub const State = enum(u8) {
    unknown,
    working,
    blocked,
    done,
    gone,

    /// The wire/display word, matching `AgentMetaState::as_str` for the four
    /// states the server publishes.
    pub fn word(state: State) []const u8 {
        return switch (state) {
            .unknown => "unknown",
            .working => "working",
            .blocked => "blocked",
            .done => "done",
            .gone => "gone",
        };
    }

    /// Open-vocabulary decode of a catalog `state` span. `idle` is a declared
    /// server state with no work and no question in it, so it lands on
    /// `unknown` here rather than acquiring a row-level meaning this
    /// projection cannot honour.
    pub fn parse(value: []const u8) State {
        if (std.mem.eql(u8, value, "working")) return .working;
        if (std.mem.eql(u8, value, "blocked")) return .blocked;
        if (std.mem.eql(u8, value, "done")) return .done;
        return .unknown;
    }

    /// Whether this state is worth interrupting a person for. Exactly one
    /// state is: an agent that cannot proceed without an answer. Work in
    /// progress and finished work are both quiet by design — see the
    /// attention rules in docs/DURABLE_WORK_ARCHITECTURE.md.
    pub fn needsAttention(state: State) bool {
        return state == .blocked;
    }
};

/// What one record says about the session, or nothing when it is narration.
pub const Evidence = enum { working, blocked, done, retract };

/// The `PhuxClientAgentRecordsKind` this host understands.
pub const RecordsKind = enum { retained, live, closed };

pub const Error = error{ Protocol, OutOfMemory };

/// One resource-catalog row, still borrowing the ABI's spans. `adopt` copies
/// everything it keeps, so an `Entry` never outlives the call it was built in.
pub const Entry = struct {
    id: RemoteId,
    /// PhuxResourceInfo has no stream generation; the host supplies its epoch.
    epoch_id: u64 = 0,
    parent: ?RemoteId = null,
    provider_name: []const u8 = "",
    native_id: []const u8 = "",
    state: []const u8 = "",
};

/// One projected agent session. Owns its text; borrows nothing.
pub const Session = struct {
    id: RemoteId,
    epoch_id: u64 = 0,
    generation: ?provider.Generation = null,
    /// Last withdrawn delivery, fencing replay while a replacement is pending.
    retired_generation: ?provider.Generation = null,
    latest_evidence: ?LatestEvidence = null,
    last_record_seq: ?u64 = null,
    parent: ?RemoteId = null,
    provider_name: []u8 = &.{},
    native_id: []u8 = &.{},
    /// The word the catalog published, decoded.
    catalog_state: State = .unknown,
    /// What this session's own records have established, once any have.
    stream_state: ?State = null,
    /// The coordinator that listed it; its refs carry it.
    provider_id: provider.ProviderId = .phux,

    /// The state to show. The stream outranks the catalog (ADR-0103 decision
    /// 5): both describe the same session, and only one of them is the agent.
    pub fn state(session: *const Session) State {
        return session.stream_state orelse session.catalog_state;
    }

    pub fn ref(session: *const Session) provider.TerminalRef {
        return .{ .provider_id = session.provider_id, .terminal_id = .{ .phux = session.id } };
    }

    pub fn parentRef(session: *const Session) ?provider.TerminalRef {
        const parent = session.parent orelse return null;
        return .{ .provider_id = session.provider_id, .terminal_id = .{ .phux = parent } };
    }

    pub fn deinit(session: *Session, gpa: std.mem.Allocator) void {
        gpa.free(session.provider_name);
        gpa.free(session.native_id);
        session.* = .{ .id = session.id, .provider_id = session.provider_id };
    }

    fn preserveEvidence(session: *Session, existing: *const Session) void {
        if (session.epoch_id != existing.epoch_id) return;
        session.stream_state = existing.stream_state;
        session.generation = existing.generation;
        session.retired_generation = existing.retired_generation;
        session.latest_evidence = existing.latest_evidence;
        session.last_record_seq = existing.last_record_seq;
    }

    fn matchesEntry(session: *const Session, entry: Entry) bool {
        return session.id.eql(entry.id) and session.epoch_id == entry.epoch_id and
            std.meta.eql(session.parent, entry.parent) and session.catalog_state == State.parse(entry.state) and
            std.mem.eql(u8, session.provider_name, entry.provider_name) and
            std.mem.eql(u8, session.native_id, entry.native_id);
    }
};

/// The agent sessions the latest attach snapshot listed, with the state their
/// streams have since established.
pub const Registry = struct {
    sessions: std.ArrayListUnmanaged(Session) = .empty,
    /// The coordinator the roster belongs to (`Host.provider_id`).
    provider_id: provider.ProviderId = .phux,

    pub fn deinit(registry: *Registry, gpa: std.mem.Allocator) void {
        registry.clear(gpa);
        registry.sessions.deinit(gpa);
        registry.* = .{ .provider_id = registry.provider_id };
    }

    pub fn clear(registry: *Registry, gpa: std.mem.Allocator) void {
        for (registry.sessions.items) |*session| session.deinit(gpa);
        registry.sessions.items.len = 0;
    }

    pub fn all(registry: *const Registry) []const Session {
        return registry.sessions.items;
    }

    pub fn find(registry: *Registry, id: RemoteId) ?*Session {
        for (registry.sessions.items) |*session| if (session.id.eql(id)) return session;
        return null;
    }

    pub fn findConst(registry: *const Registry, id: RemoteId) ?*const Session {
        for (registry.sessions.items) |*session| if (session.id.eql(id)) return session;
        return null;
    }

    pub fn catalogMatches(registry: *const Registry, entries: []const Entry) bool {
        if (registry.sessions.items.len != entries.len) return false;
        for (registry.sessions.items, entries) |*session, entry| {
            if (!session.matchesEntry(entry)) return false;
        }
        return true;
    }

    /// Replace the roster from one catalog snapshot, all or nothing.
    ///
    /// A session that survives the snapshot keeps the state its stream
    /// established: the catalog is a fresh read of server-side derivation, and
    /// letting it overwrite stream evidence would walk a blocked agent back to
    /// `working` on every unrelated attach refresh. A session the snapshot
    /// omits is gone — that is how ParentClosed reaches this projection, as an
    /// ordinary absence of the child.
    pub fn adopt(registry: *Registry, gpa: std.mem.Allocator, entries: []const Entry) Error!void {
        if (entries.len > max_sessions) return error.Protocol;
        var next: std.ArrayListUnmanaged(Session) = .empty;
        errdefer {
            for (next.items) |*session| session.deinit(gpa);
            next.deinit(gpa);
        }
        try next.ensureTotalCapacity(gpa, entries.len);
        for (entries) |entry| {
            var session = try copyEntry(gpa, entry, registry.provider_id);
            errdefer session.deinit(gpa);
            if (registry.findConst(entry.id)) |existing| session.preserveEvidence(existing);
            next.appendAssumeCapacity(session);
        }
        registry.clear(gpa);
        registry.sessions.deinit(gpa);
        registry.sessions = next;
    }

    /// Generation-less compatibility seam for existing registry consumers and
    /// fixtures. The ABI host must use applyRecordsGeneration instead. Returns
    /// whether anything a viewer can see changed.
    ///
    /// `payload` is borrowed for the duration of the call; nothing from it is
    /// retained. CLOSED retires the row, which is the effect kind's whole
    /// contract — its bytes are empty and there is no state left to derive.
    pub fn applyRecords(
        registry: *Registry,
        gpa: std.mem.Allocator,
        id: RemoteId,
        kind: RecordsKind,
        payload: []const u8,
    ) Error!bool {
        if (kind == .closed) return registry.retire(gpa, id);
        const session = registry.find(id) orelse return false;
        return applyBatch(gpa, session, kind, payload);
    }

    /// Production delivery must carry the ABI generation. Catalog membership
    /// alone cannot authorize a live frame from a replaced bootstrap.
    pub fn applyRecordsGeneration(
        registry: *Registry,
        gpa: std.mem.Allocator,
        id: RemoteId,
        generation: provider.Generation,
        kind: RecordsKind,
        payload: []const u8,
    ) Error!bool {
        const session = registry.find(id) orelse return false;
        if (!acceptGeneration(session, generation, kind)) return false;
        if (kind == .closed) {
            if (payload.len != 0) return error.Protocol;
            return registry.retire(gpa, id);
        }
        const replaced = if (session.generation) |old| !old.sameReplica(generation) else true;
        const changed = try applyBatch(gpa, session, kind, payload);
        session.generation = generation;
        return changed or replaced;
    }

    /// Inventory can reintroduce an ID before its replacement bootstrap is
    /// ready. Withdraw only the closed generation's evidence, keeping the new
    /// catalog membership and fencing delayed replay of the withdrawn stream.
    pub fn withdrawRecordsGeneration(registry: *Registry, id: RemoteId, generation: provider.Generation) bool {
        const session = registry.find(id) orelse return false;
        const current = session.generation orelse return false;
        if (!current.sameReplica(generation)) return false;
        session.retired_generation = current;
        session.generation = null;
        session.stream_state = null;
        session.latest_evidence = null;
        session.last_record_seq = null;
        return true;
    }

    /// Drop one session. Separate from `applyRecords` because a close can also
    /// reach this projection as a catalog absence, and both must land here.
    pub fn retire(registry: *Registry, gpa: std.mem.Allocator, id: RemoteId) bool {
        for (registry.sessions.items, 0..) |*session, index| {
            if (!session.id.eql(id)) continue;
            session.deinit(gpa);
            _ = registry.sessions.orderedRemove(index);
            return true;
        }
        return false;
    }

    /// The sessions running under one terminal, in catalog order. Returns the
    /// number written, which is capped by `out`.
    pub fn childrenOf(registry: *const Registry, parent: RemoteId, out: []*const Session) usize {
        var count: usize = 0;
        for (registry.sessions.items) |*session| {
            const owner = session.parent orelse continue;
            if (!owner.eql(parent)) continue;
            if (count == out.len) break;
            out[count] = session;
            count += 1;
        }
        return count;
    }

    /// Whether any agent under this terminal is waiting on a person. This is
    /// the stream-derived half of the quiet attention path: one more signal
    /// source beside the bell and the phase latches, not a second mechanism.
    pub fn parentNeedsAttention(registry: *const Registry, parent: RemoteId) bool {
        for (registry.sessions.items) |*session| {
            const owner = session.parent orelse continue;
            if (owner.eql(parent) and session.state().needsAttention()) return true;
        }
        return false;
    }
};

fn copyEntry(gpa: std.mem.Allocator, entry: Entry, provider_id: provider.ProviderId) Error!Session {
    const provider_name = try copyText(gpa, entry.provider_name);
    errdefer gpa.free(provider_name);
    const native_id = try copyText(gpa, entry.native_id);
    errdefer gpa.free(native_id);
    if (entry.state.len > max_text_bytes) return error.Protocol;
    return .{
        .id = entry.id,
        .epoch_id = entry.epoch_id,
        .parent = entry.parent,
        .provider_name = provider_name,
        .native_id = native_id,
        .catalog_state = State.parse(entry.state),
        .provider_id = provider_id,
    };
}

fn copyText(gpa: std.mem.Allocator, value: []const u8) Error![]u8 {
    if (value.len > max_text_bytes) return error.Protocol;
    if (!std.unicode.utf8ValidateSlice(value)) return error.Protocol;
    return gpa.dupe(u8, value);
}

/// Fold a JSONL batch into `state`. One JSON object per line, blank lines
/// tolerated (a batch may or may not end in a newline).
///
/// A record the vocabulary does not name is narration and moves nothing —
/// that is the open half. A line that is not a JSON object at all is a
/// protocol violation: the kernel validated every record before it reached
/// the bridge, so a malformed one means the two sides disagree about the
/// codec, and continuing would be guessing.
pub fn foldRecords(gpa: std.mem.Allocator, start: State, payload: []const u8) Error!State {
    var state = start;
    var lines = std.mem.splitScalar(u8, payload, '\n');
    while (lines.next()) |line| {
        const evidence = (try recordEvidence(gpa, line)) orelse continue;
        state = apply(state, evidence);
    }
    return state;
}

/// Apply one record's evidence. `gone` is terminal: the server accepts no
/// record after `session_end`, so nothing may walk a retracted session back
/// into claiming work.
pub fn apply(state: State, evidence: Evidence) State {
    if (state == .gone) return .gone;
    return switch (evidence) {
        .working => .working,
        .blocked => .blocked,
        .done => .done,
        .retract => .gone,
    };
}

/// What one record line says, or null when it says nothing about state.
pub fn recordEvidence(gpa: std.mem.Allocator, line: []const u8) Error!?Evidence {
    const record = try parseRecord(gpa, line) orelse return null;
    return record.evidence;
}

const Record = struct {
    evidence: ?Evidence,
    latest: ?LatestEvidence,
    seq: ?u64,
};

fn parseRecord(gpa: std.mem.Allocator, line: []const u8) Error!?Record {
    const trimmed = std.mem.trim(u8, line, " \t\r\n");
    if (trimmed.len == 0) return null;
    var parsed = std.json.parseFromSlice(std.json.Value, gpa, trimmed, .{}) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        else => return error.Protocol,
    };
    defer parsed.deinit();
    const object = switch (parsed.value) {
        .object => |value| value,
        else => return error.Protocol,
    };
    const record_type = switch (object.get("type") orelse return error.Protocol) {
        .string => |value| value,
        else => return error.Protocol,
    };
    return try decodeRecord(record_type, object);
}

fn decodeRecord(record_type: []const u8, object: std.json.ObjectMap) Error!Record {
    const data = try recordData(object);
    const seq = try unsignedField(object, "seq");
    const ts_ms = try unsignedField(object, "ts_ms");
    const evidence = evidenceFor(record_type, data);
    const latest: ?LatestEvidence = if (evidence != null) .{
        .seq = seq,
        .ts_ms = ts_ms,
        .record_type = canonicalType(record_type),
        .reason = try Reason.init(reasonFor(record_type, data)),
    } else null;
    return .{ .evidence = evidence, .latest = latest, .seq = seq };
}

fn recordData(object: std.json.ObjectMap) Error!?std.json.ObjectMap {
    const value = object.get("data") orelse return null;
    return switch (value) {
        .object => |map| map,
        .null => null,
        else => error.Protocol,
    };
}

fn unsignedField(object: std.json.ObjectMap, name: []const u8) Error!?u64 {
    const value = object.get(name) orelse return null;
    return switch (value) {
        .integer => |number| std.math.cast(u64, number) orelse error.Protocol,
        .number_string => |number| std.fmt.parseInt(u64, number, 10) catch error.Protocol,
        else => error.Protocol,
    };
}

const state_record_types = [_][]const u8{ "prompt", "tool_start", "ask", "notification", "stop", "session_end", "state" };

fn canonicalType(record_type: []const u8) []const u8 {
    for (state_record_types) |known| {
        if (std.mem.eql(u8, record_type, known)) return known;
    }
    unreachable; // Called only after evidenceFor recognized state evidence.
}

fn reasonFor(record_type: []const u8, data: ?std.json.ObjectMap) []const u8 {
    if (std.mem.eql(u8, record_type, "ask")) return stringField(data, "question") orelse "";
    if (std.mem.eql(u8, record_type, "notification")) return stringField(data, "message") orelse stringField(data, "question") orelse "";
    return stringField(data, "reason") orelse "";
}

/// ADR-0103 decision 5, stated once. Mirrors `ValidRecord::evidence` in
/// crates/phux-server/src/resource/agent_session/record.rs; the two must agree
/// because a person watching the row and a tool reading the record must not be
/// told different things about the same agent.
fn evidenceFor(record_type: []const u8, data: ?std.json.ObjectMap) ?Evidence {
    if (std.mem.eql(u8, record_type, "prompt") or std.mem.eql(u8, record_type, "tool_start")) return .working;
    if (std.mem.eql(u8, record_type, "ask")) return .blocked;
    if (std.mem.eql(u8, record_type, "notification")) {
        // Only the two kinds that stop on a human are evidence. Every other
        // notification is the agent narrating, and narration is not a reason
        // to put a marker on somebody's tab.
        return notificationEvidence(data);
    }
    if (std.mem.eql(u8, record_type, "stop")) return .done;
    if (std.mem.eql(u8, record_type, "session_end")) return .retract;
    if (std.mem.eql(u8, record_type, "state")) {
        // The REPORT_AGENT_STATE fallback, synthesized onto the stream. Its
        // word is read directly rather than re-derived.
        return stateEvidence(data);
    }
    return null;
}

fn notificationEvidence(data: ?std.json.ObjectMap) ?Evidence {
    const kind = stringField(data, "kind") orelse return null;
    if (std.mem.eql(u8, kind, "permission") or std.mem.eql(u8, kind, "elicitation")) return .blocked;
    return null;
}

fn stateEvidence(data: ?std.json.ObjectMap) ?Evidence {
    return switch (State.parse(stringField(data, "state") orelse return null)) {
        .working => .working,
        .blocked => .blocked,
        .done => .done,
        .unknown, .gone => null,
    };
}

fn acceptGeneration(session: *const Session, incoming: provider.Generation, kind: RecordsKind) bool {
    if (incoming.epoch_id != session.epoch_id) return false;
    if (session.generation) |current| {
        if (current.sameReplica(incoming)) return true;
        return kind == .retained and generationAfter(incoming, current);
    }
    if (session.retired_generation) |retired| {
        return kind != .live and generationAfter(incoming, retired);
    }
    return kind != .live;
}

fn generationAfter(incoming: provider.Generation, current: provider.Generation) bool {
    // The server uses the connection id for stream_id and monotonically
    // allocates bootstrap_id (runtime/resource_commands.rs::attach_agent_session).
    if (incoming.stream_id != current.stream_id) return incoming.stream_id > current.stream_id;
    return incoming.bootstrap_id > current.bootstrap_id;
}

const Fold = struct {
    state: ?State = null,
    latest: ?LatestEvidence = null,
    last_seq: ?u64 = null,

    fn consume(fold: *Fold, record: Record) void {
        if (record.seq) |seq| {
            if (fold.last_seq) |last| if (seq <= last) return;
            fold.last_seq = seq;
        }
        const evidence = record.evidence orelse return;
        if (fold.state == .gone) return;
        fold.state = apply(fold.state orelse .unknown, evidence);
        fold.latest = record.latest;
    }
};

fn applyBatch(gpa: std.mem.Allocator, session: *Session, kind: RecordsKind, payload: []const u8) Error!bool {
    var next: Fold = if (kind == .retained) .{} else .{
        .state = session.stream_state,
        .latest = session.latest_evidence,
        .last_seq = session.last_record_seq,
    };
    var lines = std.mem.splitScalar(u8, payload, '\n');
    while (lines.next()) |line| {
        const record = try parseRecord(gpa, line) orelse continue;
        next.consume(record);
    }
    // Publish only after validating every line, including trailing narration.
    const changed = session.stream_state != next.state or !latestEqual(session.latest_evidence, next.latest);
    session.stream_state = next.state;
    session.latest_evidence = next.latest;
    session.last_record_seq = next.last_seq;
    return changed;
}

fn latestEqual(a: ?LatestEvidence, b: ?LatestEvidence) bool {
    const left = a orelse return b == null;
    const right = b orelse return false;
    return left.seq == right.seq and left.ts_ms == right.ts_ms and
        std.mem.eql(u8, left.record_type, right.record_type) and
        left.reason.truncated == right.reason.truncated and
        std.mem.eql(u8, left.reason.slice(), right.reason.slice());
}

fn stringField(data: ?std.json.ObjectMap, name: []const u8) ?[]const u8 {
    const object = data orelse return null;
    return switch (object.get(name) orelse return null) {
        .string => |value| value,
        else => null,
    };
}

// ------------------------------------------------------------------ tests

const testing = std.testing;

test "repeated blocked evidence invalidates the row without changing attention" {
    const gpa = testing.allocator;
    var registry: Registry = .{};
    defer registry.deinit(gpa);
    try registry.adopt(gpa, &.{.{ .id = localId(9), .state = "working" }});
    try testing.expect(try registry.applyRecords(gpa, localId(9), .live, "{\"seq\":1,\"type\":\"ask\",\"data\":{\"question\":\"first?\"}}"));
    try testing.expect(try registry.applyRecords(gpa, localId(9), .live, "{\"seq\":2,\"type\":\"ask\",\"data\":{\"question\":\"second?\"}}"));
    try testing.expectEqual(State.blocked, registry.find(localId(9)).?.state());
}

test "retained replacement discards evidence absent from the new backlog" {
    const gpa = testing.allocator;
    var registry: Registry = .{};
    defer registry.deinit(gpa);
    try registry.adopt(gpa, &.{.{ .id = localId(9), .state = "working" }});
    _ = try registry.applyRecords(gpa, localId(9), .live, "{\"type\":\"ask\"}");
    _ = try registry.applyRecords(gpa, localId(9), .retained, "{\"type\":\"provider_raw\"}");
    try testing.expectEqual(State.working, registry.find(localId(9)).?.state());
}

test "generation replacement fences stale live retained and closed delivery" {
    const gpa = testing.allocator;
    var registry: Registry = .{};
    defer registry.deinit(gpa);
    const id = localId(9);
    try registry.adopt(gpa, &.{.{ .id = id, .epoch_id = 1, .state = "working" }});
    const old: provider.Generation = .{ .epoch_id = 1, .stream_id = 4, .bootstrap_id = 5 };
    var current = old;
    current.bootstrap_id += 1;
    const ask = "{\"seq\":8,\"ts_ms\":90,\"type\":\"ask\",\"data\":{\"question\":\"old?\"}}";
    // A live frame cannot establish a generation without its retained barrier.
    try testing.expect(!try registry.applyRecordsGeneration(gpa, id, old, .live, ask));
    try testing.expect(try registry.applyRecordsGeneration(gpa, id, old, .retained, ask));
    try testing.expect(try registry.applyRecordsGeneration(gpa, id, current, .retained, "{\"seq\":9,\"type\":\"provider_raw\"}"));
    try testing.expectEqual(State.working, registry.find(id).?.state());
    try testing.expect(registry.find(id).?.latest_evidence == null);
    for ([_]RecordsKind{ .live, .retained, .closed }) |kind| {
        try testing.expect(!try registry.applyRecordsGeneration(gpa, id, old, kind, ask));
    }
    try testing.expectEqual(@as(usize, 1), registry.all().len);
    const new_ask = "{\"seq\":10,\"ts_ms\":100,\"type\":\"ask\",\"data\":{\"question\":\"new?\"}}";
    try testing.expect(try registry.applyRecordsGeneration(gpa, id, current, .live, new_ask));
    try testing.expect(!try registry.applyRecordsGeneration(gpa, id, current, .live, ask));
    const session = registry.find(id).?;
    try testing.expectEqualStrings("new?", session.latest_evidence.?.reason.slice());
    try testing.expectEqual(@as(?u64, 10), session.latest_evidence.?.seq);
    try testing.expectEqual(@as(?u64, 100), session.latest_evidence.?.ts_ms);
    try testing.expectEqualStrings("ask", session.latest_evidence.?.record_type);
    // Catalog refreshes preserve only evidence from the same client epoch.
    try registry.adopt(gpa, &.{.{ .id = id, .epoch_id = 1, .state = "working" }});
    try testing.expectEqualStrings("new?", registry.find(id).?.latest_evidence.?.reason.slice());
    try registry.adopt(gpa, &.{.{ .id = id, .epoch_id = 2, .state = "working" }});
    try testing.expect(registry.find(id).?.latest_evidence == null);
    try testing.expect(!try registry.applyRecordsGeneration(gpa, id, current, .closed, ""));
    current.epoch_id = 2;
    try testing.expect(try registry.applyRecordsGeneration(gpa, id, current, .retained, new_ask));
    try testing.expectError(error.Protocol, registry.applyRecordsGeneration(gpa, id, current, .closed, new_ask));
    try testing.expect(try registry.applyRecordsGeneration(gpa, id, current, .closed, ""));
    try testing.expectEqual(@as(usize, 0), registry.all().len);
    // Inventory withdrawal is not permanent identity death: a resource may
    // reappear with the same ID and a fresh retained generation.
    try registry.adopt(gpa, &.{.{ .id = id, .epoch_id = 2, .state = "working" }});
    current.bootstrap_id += 1;
    try testing.expect(try registry.applyRecordsGeneration(gpa, id, current, .retained, ""));
    try testing.expectEqual(State.working, registry.find(id).?.state());
    try testing.expect(registry.find(id).?.latest_evidence == null);
    try testing.expect(registry.find(id).?.last_record_seq == null);
}

test "malformed trailing records leave evidence sequence and generation atomic" {
    const gpa = testing.allocator;
    var registry: Registry = .{};
    defer registry.deinit(gpa);
    const id = localId(9);
    try registry.adopt(gpa, &.{.{ .id = id, .state = "working" }});
    const generation: provider.Generation = .{ .stream_id = 1, .bootstrap_id = 1 };
    const ask = "{\"seq\":1,\"ts_ms\":12,\"type\":\"ask\",\"data\":{\"question\":\"keep?\"}}";
    _ = try registry.applyRecordsGeneration(gpa, id, generation, .retained, ask);
    const invalid = [_][]const u8{
        "{\"type\":}",
        "{\"type\":\"provider_raw\",\"data\":[]}",
        "{\"seq\":-1,\"type\":\"stop\"}",
        "{\"seq\":1.5,\"type\":\"stop\"}",
        "{\"ts_ms\":\"later\",\"type\":\"stop\"}",
    };
    for (invalid) |trailing| {
        const batch = try std.fmt.allocPrint(gpa, "{{\"seq\":2,\"type\":\"stop\"}}\n{s}", .{trailing});
        defer gpa.free(batch);
        try testing.expectError(error.Protocol, registry.applyRecordsGeneration(gpa, id, generation, .live, batch));
        var next = generation;
        next.bootstrap_id += 1;
        try testing.expectError(error.Protocol, registry.applyRecordsGeneration(gpa, id, next, .retained, batch));
        const session = registry.find(id).?;
        try testing.expectEqual(State.blocked, session.state());
        try testing.expectEqualStrings("keep?", session.latest_evidence.?.reason.slice());
        try testing.expectEqual(@as(?u64, 1), session.last_record_seq);
        try testing.expect(session.generation.?.sameReplica(generation));
    }
    // A failed batch must not consume seq=2; a subsequent valid one applies.
    try testing.expect(try registry.applyRecordsGeneration(gpa, id, generation, .live, "{\"seq\":2,\"type\":\"stop\"}"));
    try testing.expectEqual(State.done, registry.find(id).?.state());
}

test "latest evidence owns UTF8 safe visibly truncated reason and optional metadata" {
    const gpa = testing.allocator;
    var registry: Registry = .{};
    defer registry.deinit(gpa);
    const id = localId(9);
    try registry.adopt(gpa, &.{.{ .id = id }});
    // Three-byte characters straddle the budget boundary reserved for '...'.
    const text = "\u{754c}" ** (max_reason_bytes / 3 + 1);
    const payload = try std.fmt.allocPrint(gpa, "{{\"seq\":18446744073709551615,\"ts_ms\":18446744073709551615,\"type\":\"notification\",\"data\":{{\"kind\":\"permission\",\"message\":\"{s}\"}}}}", .{text});
    _ = try registry.applyRecords(gpa, id, .retained, payload);
    @memset(payload, 'x');
    gpa.free(payload);
    const session = registry.find(id).?;
    const latest = &session.latest_evidence.?;
    try testing.expect(latest.reason.truncated);
    const prefix_bytes = ((max_reason_bytes - 3) / 3) * 3;
    try testing.expectEqual(prefix_bytes + 3, latest.reason.len);
    try testing.expectEqualStrings(text[0..prefix_bytes], latest.reason.slice()[0..prefix_bytes]);
    try testing.expect(std.unicode.utf8ValidateSlice(latest.reason.slice()));
    try testing.expect(std.mem.endsWith(u8, latest.reason.slice(), "..."));
    try testing.expectEqual(@as(?u64, std.math.maxInt(u64)), latest.seq);
    try testing.expectEqual(@as(?u64, std.math.maxInt(u64)), latest.ts_ms);
    // Narration changes neither evidence nor the explicit truncation marker.
    try testing.expect(!try registry.applyRecords(gpa, id, .live, "{\"type\":\"provider_raw\"}"));
    try testing.expectEqualStrings("notification", session.latest_evidence.?.record_type);
    _ = try registry.applyRecords(gpa, id, .retained, "{\"type\":\"state\",\"data\":{\"state\":\"blocked\",\"reason\":\"retry\"}}");
    try testing.expectEqualStrings("retry", session.latest_evidence.?.reason.slice());
    try testing.expect(!session.latest_evidence.?.reason.truncated);
    try testing.expect(session.latest_evidence.?.seq == null);
    try testing.expect(session.latest_evidence.?.ts_ms == null);
    _ = try registry.applyRecords(gpa, id, .live, "{\"type\":\"ask\"}");
    try testing.expectEqualStrings("", session.latest_evidence.?.reason.slice());
}

fn localId(id: u32) RemoteId {
    return RemoteId.fromPhux(0, id, "") catch unreachable;
}

test "decision 5 maps every state-bearing record and nothing else" {
    const gpa = testing.allocator;
    try testing.expectEqual(Evidence.working, (try recordEvidence(gpa, "{\"seq\":1,\"ts_ms\":1,\"type\":\"prompt\",\"data\":{}}")).?);
    try testing.expectEqual(Evidence.working, (try recordEvidence(gpa, "{\"type\":\"tool_start\",\"data\":{\"tool_name\":\"Bash\"}}")).?);
    try testing.expectEqual(Evidence.blocked, (try recordEvidence(gpa, "{\"type\":\"ask\",\"data\":{\"question\":\"ok?\"}}")).?);
    try testing.expectEqual(Evidence.blocked, (try recordEvidence(gpa, "{\"type\":\"notification\",\"data\":{\"kind\":\"permission\"}}")).?);
    try testing.expectEqual(Evidence.blocked, (try recordEvidence(gpa, "{\"type\":\"notification\",\"data\":{\"kind\":\"elicitation\"}}")).?);
    try testing.expectEqual(Evidence.done, (try recordEvidence(gpa, "{\"type\":\"stop\",\"data\":{}}")).?);
    try testing.expectEqual(Evidence.retract, (try recordEvidence(gpa, "{\"type\":\"session_end\",\"data\":{}}")).?);
    try testing.expectEqual(Evidence.blocked, (try recordEvidence(gpa, "{\"type\":\"state\",\"data\":{\"state\":\"blocked\"}}")).?);

    // Narration, and a vocabulary this build does not know: both move nothing.
    try testing.expectEqual(@as(?Evidence, null), try recordEvidence(gpa, "{\"type\":\"notification\",\"data\":{\"kind\":\"progress\"}}"));
    try testing.expectEqual(@as(?Evidence, null), try recordEvidence(gpa, "{\"type\":\"notification\",\"data\":{}}"));
    try testing.expectEqual(@as(?Evidence, null), try recordEvidence(gpa, "{\"type\":\"session_start\",\"data\":{}}"));
    try testing.expectEqual(@as(?Evidence, null), try recordEvidence(gpa, "{\"type\":\"tool_end\",\"data\":{}}"));
    try testing.expectEqual(@as(?Evidence, null), try recordEvidence(gpa, "{\"type\":\"provider_raw\",\"data\":{}}"));
    try testing.expectEqual(@as(?Evidence, null), try recordEvidence(gpa, "{\"type\":\"invented_later\",\"data\":{}}"));
    try testing.expectEqual(@as(?Evidence, null), try recordEvidence(gpa, "{\"type\":\"state\",\"data\":{\"state\":\"idle\"}}"));

    // A record whose codec disagrees with ours is a protocol failure, not a
    // shrug: the kernel validated it, so disagreement means one of us is wrong.
    try testing.expectError(error.Protocol, recordEvidence(gpa, "{\"type\":}"));
    try testing.expectError(error.Protocol, recordEvidence(gpa, "[\"prompt\"]"));
    try testing.expectError(error.Protocol, recordEvidence(gpa, "{\"seq\":1}"));
    try testing.expectError(error.Protocol, recordEvidence(gpa, "{\"type\":7}"));
}

test "a retained batch folds to its last state-bearing record" {
    const gpa = testing.allocator;
    const batch =
        "{\"seq\":1,\"ts_ms\":10,\"type\":\"session_start\",\"data\":{\"provider\":\"claude\"}}\n" ++
        "{\"seq\":2,\"ts_ms\":20,\"type\":\"prompt\",\"data\":{\"chars\":4}}\n" ++
        "{\"seq\":3,\"ts_ms\":30,\"type\":\"tool_start\",\"data\":{\"tool_name\":\"Bash\"}}\n" ++
        "{\"seq\":4,\"ts_ms\":40,\"type\":\"tool_end\",\"data\":{}}\n" ++
        "{\"seq\":5,\"ts_ms\":50,\"type\":\"ask\",\"data\":{\"question\":\"run it?\"}}\n";
    try testing.expectEqual(State.blocked, try foldRecords(gpa, .unknown, batch));
    // Trailing narration after the ask does not clear it; only new evidence does.
    try testing.expectEqual(State.blocked, try foldRecords(gpa, .blocked, "{\"type\":\"tool_end\",\"data\":{}}\n"));
    try testing.expectEqual(State.working, try foldRecords(gpa, .blocked, "{\"type\":\"prompt\",\"data\":{}}\n"));
    try testing.expectEqual(State.done, try foldRecords(gpa, .working, "{\"type\":\"stop\",\"data\":{}}"));
    try testing.expectEqual(State.unknown, try foldRecords(gpa, .unknown, ""));
}

test "session_end is terminal and nothing on the stream walks it back" {
    const gpa = testing.allocator;
    const ended = try foldRecords(gpa, .working, "{\"type\":\"session_end\",\"data\":{}}\n");
    try testing.expectEqual(State.gone, ended);
    try testing.expectEqual(State.gone, try foldRecords(gpa, ended, "{\"type\":\"prompt\",\"data\":{}}\n"));
    try testing.expectEqual(State.gone, apply(.gone, .blocked));
}

test "adopting a catalog keeps stream evidence and drops what the snapshot omits" {
    const gpa = testing.allocator;
    var registry: Registry = .{};
    defer registry.deinit(gpa);

    try registry.adopt(gpa, &.{
        .{ .id = localId(9), .parent = localId(7), .provider_name = "claude", .native_id = "abc", .state = "working" },
        .{ .id = localId(11), .parent = localId(7), .provider_name = "codex", .state = "idle" },
    });
    try testing.expectEqual(@as(usize, 2), registry.all().len);
    try testing.expectEqualStrings("claude", registry.all()[0].provider_name);
    try testing.expectEqualStrings("abc", registry.all()[0].native_id);
    try testing.expectEqual(State.working, registry.all()[0].state());
    // `idle` is a word this projection does not carry, and is not guessed at.
    try testing.expectEqual(State.unknown, registry.all()[1].state());

    try testing.expect(try registry.applyRecords(gpa, localId(9), .live, "{\"type\":\"ask\",\"data\":{\"question\":\"go?\"}}\n"));
    try testing.expectEqual(State.blocked, registry.find(localId(9)).?.state());

    // A refresh that still publishes the server's older `working` must not
    // walk the blocked row back.
    try registry.adopt(gpa, &.{
        .{ .id = localId(9), .parent = localId(7), .provider_name = "claude", .native_id = "abc", .state = "working" },
    });
    try testing.expectEqual(@as(usize, 1), registry.all().len);
    try testing.expectEqual(State.blocked, registry.find(localId(9)).?.state());
    try testing.expectEqual(@as(?*Session, null), registry.find(localId(11)));
}

test "CLOSED retires the row and a stream for an unknown resource changes nothing" {
    const gpa = testing.allocator;
    var registry: Registry = .{};
    defer registry.deinit(gpa);

    try registry.adopt(gpa, &.{.{ .id = localId(9), .parent = localId(7), .provider_name = "claude", .state = "working" }});
    try testing.expect(!try registry.applyRecords(gpa, localId(404), .live, "{\"type\":\"ask\",\"data\":{}}\n"));
    try testing.expectEqual(@as(usize, 1), registry.all().len);

    try testing.expect(try registry.applyRecords(gpa, localId(9), .closed, ""));
    try testing.expectEqual(@as(usize, 0), registry.all().len);
    try testing.expect(!try registry.applyRecords(gpa, localId(9), .closed, ""));
}

test "rows hang under their parent and only a blocked one asks for attention" {
    const gpa = testing.allocator;
    var registry: Registry = .{};
    defer registry.deinit(gpa);

    try registry.adopt(gpa, &.{
        .{ .id = localId(9), .parent = localId(7), .provider_name = "claude", .state = "working" },
        .{ .id = localId(10), .parent = localId(8), .provider_name = "codex", .state = "done" },
        .{ .id = localId(11), .parent = localId(7), .provider_name = "codex", .state = "done" },
        .{ .id = localId(12), .provider_name = "orphan", .state = "blocked" },
    });

    var rows: [max_sessions]*const Session = undefined;
    try testing.expectEqual(@as(usize, 2), registry.childrenOf(localId(7), &rows));
    try testing.expectEqual(localId(9).id, rows[0].id.id);
    try testing.expectEqual(localId(11).id, rows[1].id.id);
    try testing.expect(rows[0].parentRef().?.eql(.{ .provider_id = .phux, .terminal_id = .{ .phux = localId(7) } }));
    try testing.expect(rows[0].ref().eql(.{ .provider_id = .phux, .terminal_id = .{ .phux = localId(9) } }));

    // A parentless session is projected nowhere; it cannot raise attention on
    // a terminal it does not name.
    try testing.expect(!registry.parentNeedsAttention(localId(7)));
    try testing.expect(!registry.parentNeedsAttention(localId(8)));
    _ = try registry.applyRecords(gpa, localId(11), .retained, "{\"type\":\"notification\",\"data\":{\"kind\":\"permission\"}}\n");
    try testing.expect(registry.parentNeedsAttention(localId(7)));

    var one: [1]*const Session = undefined;
    try testing.expectEqual(@as(usize, 1), registry.childrenOf(localId(7), &one));
}

test "catalog text is copied, bounded, and validated" {
    const gpa = testing.allocator;
    var registry: Registry = .{};
    defer registry.deinit(gpa);

    var name = "claude".*;
    try registry.adopt(gpa, &.{.{ .id = localId(9), .parent = localId(7), .provider_name = &name }});
    name[0] = 'x';
    try testing.expectEqualStrings("claude", registry.all()[0].provider_name);

    const long = [_]u8{'x'} ** (max_text_bytes + 1);
    try testing.expectError(error.Protocol, registry.adopt(gpa, &.{.{ .id = localId(9), .provider_name = &long }}));
    try testing.expectError(error.Protocol, registry.adopt(gpa, &.{.{ .id = localId(9), .native_id = &[_]u8{0xff} }}));
    try testing.expectError(error.Protocol, registry.adopt(gpa, &.{.{ .id = localId(9), .state = &long }}));
    // A refused adoption leaves the last good roster standing.
    try testing.expectEqual(@as(usize, 1), registry.all().len);
    try testing.expectEqualStrings("claude", registry.all()[0].provider_name);
}
