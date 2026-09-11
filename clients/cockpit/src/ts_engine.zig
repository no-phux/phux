//! Module root for the shipping TypeScript-core graph. A Zig module may only
//! import files below its own root, so the native engine is exposed to the
//! root extension as `cockpit_engine` rather than through relative paths.
const std = @import("std");

pub const engine = @import("cockpit/native/ts_engine.zig");
pub const protocol = @import("cockpit/native/ts_protocol.zig");
pub const snapshot = @import("cockpit/native/ts_snapshot.zig");
pub const projection = @import("cockpit/native/workspace_projection.zig");
pub const layout = @import("cockpit/layout.zig");
pub const scene = @import("cockpit/native/scene.zig");
pub const startup = @import("cockpit/startup.zig");
pub const attachPhuxProvider = @import("cockpit/model.zig").attachPhuxProvider;
pub const phux_enabled = @import("cockpit/phux_support.zig").phux_enabled;
pub const command_results = @import("cockpit/native/command_results.zig");
pub const remote_hosts = @import("cockpit/native/remote_hosts.zig");
pub const directory_picker = @import("cockpit/native/directory_picker.zig");
pub const session_commands = @import("cockpit/native/session_commands.zig");
pub const phux_peer_channel_key = @import("cockpit/phux_support.zig").phux_peer_channel_key;
pub const PhuxProvider = @import("cockpit/phux_support.zig").PhuxProvider;
pub const TerminalRef = @import("provider_contract").TerminalRef;
pub const RemoteResourceId = @import("provider_contract").RemoteResourceId;
/// -Dmeasure=true gated diagnostics, the same channel the Zig graph's tests use.
pub const measured = @import("tests/measured.zig");
pub const Engine = engine.Engine;
pub const durable_tests = @import("cockpit/durable_creation_tests.zig");
pub const catalog_tests = @import("cockpit/catalog_navigation_tests.zig");
pub const shared_mutation_tests = @import("cockpit/shared_mutations_test.zig");
pub const NoShells = engine.NoShells;
pub const selection_autoscroll_timer_id: u64 = @import("cockpit/app_types.zig").selection_autoscroll_timer_id;
pub const selection_autoscroll_interval_ns: u64 = 15 * std.time.ns_per_ms;
pub const topology_state_file_key = @import("cockpit/update.zig").topology_state_file_key;
pub const topology_persist_timer_key = @import("cockpit/update.zig").topology_persist_timer_key;
pub const topology_persist_debounce_ms = @import("cockpit/update.zig").topology_persist_debounce_ms;
pub const phux_channel_key = @import("cockpit/phux_support.zig").phux_channel_key;
pub const pointer_channel_key = @import("cockpit/phux_support.zig").pointer_channel_key;

test {
    std.testing.refAllDecls(@This());
}
