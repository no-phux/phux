const std = @import("std");

/// Reuse Cockpit's production module graph; this opt-in step never launches an app.
pub fn add(b: *std.Build, provider: *std.Build.Module) void {
    const root = b.createModule(.{
        .root_source_file = b.path("tests/everyday-remote/provider.zig"),
        .target = provider.resolved_target.?,
        .optimize = provider.optimize.?,
    });
    root.addImport("phux_provider", provider);
    root.addImport("provider_contract", provider.import_table.get("provider_contract").?);
    const probe = b.addTest(.{ .name = "everyday-remote-provider", .root_module = root });
    const install = b.addInstallArtifact(probe, .{});
    b.step("everyday-remote-provider", "Build headless enrolled-server provider probe").dependOn(&install.step);
}
