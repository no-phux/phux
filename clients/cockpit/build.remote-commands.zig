//! Focused remote presentation regression gate on the shipping app graph.
const std = @import("std");
const shipping = @import("build.zig");

pub fn build(b: *std.Build) void {
    shipping.build(b);
    const gate = b.step("remote-commands-test", "Run shipping remote presentation regressions");
    var visited: std.AutoHashMapUnmanaged(*std.Build.Step, void) = .empty;
    collect(b, gate, &b.top_level_steps.get("test").?.step, &visited);
    const verdict = b.addSystemCommand(&.{ "/usr/bin/printf", "remote-commands-test: PASS (scoped)\n  source root:   %s\n", b.build_root.path.? });
    verdict.step.dependOn(gate);
    b.default_step = &verdict.step;
    b.step("remote-commands-check", "Remote commands with source-root verdict").dependOn(&verdict.step);
}

fn collect(b: *std.Build, gate: *std.Build.Step, step: *std.Build.Step, visited: *std.AutoHashMapUnmanaged(*std.Build.Step, void)) void {
    const found = visited.getOrPut(b.allocator, step) catch @panic("out of memory");
    if (found.found_existing) return;
    if (step.cast(std.Build.Step.Compile)) |compile| {
        if (compile.kind == .@"test") {
            compile.filters = &.{"remote presentation"};
            gate.dependOn(&b.addRunArtifact(compile).step);
        }
    }
    for (step.dependencies.items) |dependency| collect(b, gate, dependency, visited);
}
