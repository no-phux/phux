//! Module root for the TypeScript-core build graph. The extension module
//! that fronts the compiled core lives under `typescript-spike/`, and a Zig
//! module may only import files below its own root, so the engine is
//! exposed to it as a module rooted here (`cockpit_engine` in build.zig)
//! rather than by relative path.
const std = @import("std");

pub const engine = @import("cockpit/native/ts_engine.zig");
pub const protocol = @import("cockpit/native/ts_protocol.zig");
pub const snapshot = @import("cockpit/native/ts_snapshot.zig");
pub const projection = @import("cockpit/native/workspace_projection.zig");
pub const layout = @import("cockpit/layout.zig");
pub const scene = @import("cockpit/native/scene.zig");
pub const startup = @import("cockpit/startup.zig");
/// -Dmeasure=true gated diagnostics, the same channel the Zig graph's tests use.
pub const measured = @import("tests/measured.zig");
pub const Engine = engine.Engine;
pub const NoShells = engine.NoShells;
pub const selection_autoscroll_timer_id: u64 = @import("cockpit/app_types.zig").selection_autoscroll_timer_id;
pub const selection_autoscroll_interval_ns: u64 = 15 * std.time.ns_per_ms;

test {
    std.testing.refAllDecls(@This());
}
