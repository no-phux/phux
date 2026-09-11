//! Calm, per-terminal status and owner-fenced bell consumption.
const model_module = @import("../model.zig");
const support = @import("../phux_support.zig");

pub const State = enum(u8) { quiet, attaching, recovering, frozen, unavailable, ended, history_loading, history_available };

pub fn state(model: *const model_module.Model, ref: support.TerminalRef) State {
    if (comptime !support.phux_enabled) return .quiet;
    if (support.providerKind(ref) != .phux) return .quiet;
    const remote = model.phuxForRefConst(ref) orelse return .unavailable;
    if (model.attachmentPending(ref)) return .recovering;
    const phase = remote.phase(ref) orelse return .attaching;
    if (phase != .live) return phaseState(phase);
    const presentation = model.remotePresentation(ref) orelse return .attaching;
    return historyState(presentation);
}

fn phaseState(phase: @import("provider_contract").Phase) State {
    return switch (phase) {
        .starting, .attaching => .attaching,
        .reconnecting => .recovering,
        .frozen => .frozen,
        .failed, .tombstoned => .unavailable,
        .ended => .ended,
        .live => .quiet,
    };
}

fn historyState(presentation: model_module.Presentation) State {
    if (presentation.history_loading) return .history_loading;
    if (presentation.history_has_more and presentation.history_viewport_offset > 0) return .history_available;
    return .quiet;
}

pub fn label(value: State) []const u8 {
    return switch (value) {
        .quiet => "",
        .attaching => "Loading terminal",
        .recovering => "Recovering terminal: waiting for snapshot",
        .frozen => "Terminal frozen: waiting for recovery",
        .unavailable => "Terminal unavailable",
        .ended => "Terminal ended",
        .history_loading => "Loading earlier history",
        .history_available => "Earlier history available",
    };
}

pub fn windowState(model: *const model_module.Model, window: usize) State {
    const workspace = model.wsAtConst(window) orelse return .quiet;
    const ref = workspace.focusedTerminalRef() orelse return .quiet;
    return state(model, ref);
}
