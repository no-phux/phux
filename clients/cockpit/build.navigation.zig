//! Focused catalog/codec contract gate on the shipping compilation graph.
//! Engine/bridge integration tests belong to the parent gate; this gate keeps
//! navigation independently executable while those hooks are being integrated.
const std = @import("std");
const shipping = @import("build.zig");

pub fn build(b: *std.Build) void {
    shipping.build(b);
    const gate = b.step("navigation-test", "Compile shipping TS and run native navigation contracts");
    const test_step = &b.top_level_steps.get("test").?.step;
    var visited: std.AutoHashMapUnmanaged(*std.Build.Step, void) = .empty;
    collect(b, gate, test_step, &visited);
    const verdict = b.addSystemCommand(&.{ "/usr/bin/printf", "navigation-test: PASS\n  source root:   %s\n", b.build_root.path.? });
    verdict.step.dependOn(gate);
    b.default_step = &verdict.step;
    const named = b.step("navigation-check", "Navigation tests with source-root verdict");
    named.dependOn(&verdict.step);
}

fn collect(b: *std.Build, gate: *std.Build.Step, step: *std.Build.Step, visited: *std.AutoHashMapUnmanaged(*std.Build.Step, void)) void {
    const found = visited.getOrPut(b.allocator, step) catch @panic("out of memory");
    if (found.found_existing) return;
    if (step.cast(std.Build.Step.Compile)) |compile| {
        if (compile.kind == .@"test") {
            compile.filters = &.{"navigation "};
            const run = b.addRunArtifact(compile);
            gate.dependOn(&run.step);
            // Imported module tests do not run as part of the extension's root.
            // Give the exact shipping engine module its own test artifact.
            if (compile.root_module.import_table.get("cockpit_engine")) |engine| {
                const contracts = b.addTest(.{ .name = "navigation-contracts", .root_module = engine, .filters = &.{"navigation "} });
                gate.dependOn(&b.addRunArtifact(contracts).step);
            }
        }
    }
    for (step.dependencies.items) |dependency| collect(b, gate, dependency, visited);
}
