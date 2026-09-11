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

/// Roster ceiling, matching the ABI's resource-catalog bound. Discovery is
/// bounded the same way terminal discovery is; live replicas are a separate,
/// much smaller budget that agent sessions never draw on.
pub const max_sessions: usize = provider.workspace.max_terminals;

/// Byte ceiling for one `provider` or `native_id` span. Well under the
/// workspace text bound: these are slugs (`claude`, `codex`) and producer-side
/// identifiers, not titles, and the roster keeps one copy of each per session.
pub const max_text_bytes: usize = 256;

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
    parent: ?RemoteId = null,
    provider_name: []const u8 = "",
    native_id: []const u8 = "",
    state: []const u8 = "",
};

/// One projected agent session. Owns its text; borrows nothing.
pub const Session = struct {
    id: RemoteId,
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
            if (registry.findConst(entry.id)) |existing| session.stream_state = existing.stream_state;
            next.appendAssumeCapacity(session);
        }
        registry.clear(gpa);
        registry.sessions.deinit(gpa);
        registry.sessions = next;
    }

    /// Fold one AGENT_RECORDS effect into the addressed session. Returns
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
        const before = session.state();
        session.stream_state = try foldRecords(gpa, session.state(), payload);
        return session.state() != before;
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
    const data: ?std.json.ObjectMap = if (object.get("data")) |value| switch (value) {
        .object => |map| map,
        else => null,
    } else null;
    return evidenceFor(record_type, data);
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
        const kind = stringField(data, "kind") orelse return null;
        if (std.mem.eql(u8, kind, "permission") or std.mem.eql(u8, kind, "elicitation")) return .blocked;
        return null;
    }
    if (std.mem.eql(u8, record_type, "stop")) return .done;
    if (std.mem.eql(u8, record_type, "session_end")) return .retract;
    if (std.mem.eql(u8, record_type, "state")) {
        // The REPORT_AGENT_STATE fallback, synthesized onto the stream. Its
        // word is read directly rather than re-derived.
        return switch (State.parse(stringField(data, "state") orelse return null)) {
            .working => .working,
            .blocked => .blocked,
            .done => .done,
            .unknown, .gone => null,
        };
    }
    return null;
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
