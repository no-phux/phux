// The default app graph uses the local provider; -Dphux-enabled=true
// materializes the Phux lane (C header, static archive, Objective-C, AppKit).
// `zig build test` compiles and runs the Phux modules whenever the client FFI
// is found, independent of -Dphux-enabled, and ends with a verdict naming
// what was compiled (the exit code stays authoritative).
const std = @import("std");
const native_sdk = @import("native_sdk");

/// The SDK package's TypeScript frontend needs `scriptc`, which the tarball
/// pin does not carry; install it once at configure time for the resolved
/// package.
fn ensureTsToolchain(b: *std.Build, dependency: *std.Build.Dependency) void {
    const root = dependency.builder.build_root.path orelse return;
    const core_dir = b.pathJoin(&.{ root, "packages", "core" });
    const probe = b.pathJoin(&.{ core_dir, "node_modules", "scriptc", "dist", "main.js" });
    if (std.Io.Dir.cwd().access(b.graph.io, probe, .{})) |_| {
        return;
    } else |_| {}
    std.debug.print("typescript-core: installing the SDK package's TypeScript toolchain once in {s}\n", .{core_dir});
    const result = std.process.run(b.allocator, b.graph.io, .{
        .argv = &.{ "npm", "ci", "--include=dev" },
        .cwd = .{ .path = core_dir },
    }) catch |err| {
        std.debug.print("typescript-core: could not run npm ci in {s}: {s}. Install Node 24+ and run it by hand.\n", .{ core_dir, @errorName(err) });
        return;
    };
    switch (result.term) {
        .exited => |code| if (code == 0) return,
        else => {},
    }
    std.debug.print("typescript-core: npm ci failed in {s}:\n{s}\n", .{ core_dir, result.stderr });
}

fn createTsEngine(
    b: *std.Build,
    target: std.Build.ResolvedTarget,
    optimize: std.builtin.OptimizeMode,
    sdk_module: *std.Build.Module,
    measure: bool,
    phux_enabled: bool,
    ffi: ?PhuxFfi,
) *std.Build.Module {
    const contract = b.createModule(.{
        .root_source_file = b.path("src/providers/contract.zig"),
        .target = target,
        .optimize = optimize,
    });
    contract.addImport("native_sdk", sdk_module);
    const phux_options = b.addOptions();
    phux_options.addOption(bool, "enabled", phux_enabled);
    const test_options = b.addOptions();
    test_options.addOption(bool, "measure", measure);
    const ghostty = b.dependency("ghostty", .{
        .target = target,
        .optimize = optimize,
        .simd = false,
        .@"emit-xcframework" = false,
        .@"emit-macos-app" = false,
    });
    const engine = b.createModule(.{
        .root_source_file = b.path("src/ts_engine.zig"),
        .target = target,
        .optimize = optimize,
    });
    engine.addImport("native_sdk", sdk_module);
    engine.addImport("provider_contract", contract);
    engine.addImport("phux_options", phux_options.createModule());
    engine.addImport("test_options", test_options.createModule());
    engine.addImport("ghostty-vt", ghostty.module("ghostty-vt"));
    if (phux_enabled) {
        const modules = createPhuxModules(b, target, optimize, sdk_module, contract, ffi.?);
        attachPhuxModules(b, engine, modules);
    }
    return engine;
}

fn addTsEngineModules(
    b: *std.Build,
    artifacts: native_sdk.AppArtifacts,
    measure: bool,
    phux_enabled: bool,
    ffi: ?PhuxFfi,
) void {
    const extension_tests = artifacts.extension_tests orelse
        @panic("native-sdk app graph did not expose the extension test artifact");
    const roots = [_]*std.Build.Module{
        artifacts.extension orelse @panic("native-sdk app graph did not expose the extension module"),
        artifacts.test_extension orelse @panic("native-sdk app graph did not expose the test extension module"),
        extension_tests.root_module,
    };
    for (roots, 0..) |root, index| {
        var seen = false;
        for (roots[0..index]) |earlier| seen = seen or earlier == root;
        if (seen) continue;
        const target = root.resolved_target.?;
        const optimize = root.optimize.?;
        const sdk_module = root.import_table.get("native_sdk") orelse
            @panic("native-sdk extension module did not expose the SDK root module");
        root.addImport("cockpit_engine", createTsEngine(
            b,
            target,
            optimize,
            sdk_module,
            measure,
            phux_enabled,
            ffi,
        ));
    }
}

/// Typecheck DisabledPhuxProvider inside the phux-enabled test graph so CI
/// does not run a second `zig build test` just for compile coverage (phux-q0i3,
/// phux-9ajd). Compile only: do not re-run the extension tests.
fn addDisabledProviderCompileCheck(
    b: *std.Build,
    artifacts: native_sdk.AppArtifacts,
    test_step: *std.Build.Step,
    measure: bool,
) void {
    const extension = artifacts.extension orelse
        @panic("native-sdk app graph did not expose the extension module");
    const target = extension.resolved_target.?;
    const optimize = extension.optimize.?;
    const sdk_module = extension.import_table.get("native_sdk") orelse
        @panic("native-sdk extension module did not expose the SDK root module");
    const core = extension.import_table.get("core") orelse
        @panic("native-sdk extension module did not expose the core module");
    const check = b.createModule(.{
        .root_source_file = b.path("src/native_extension.zig"),
        .target = target,
        .optimize = optimize,
    });
    check.link_libc = true;
    check.addImport("native_sdk", sdk_module);
    check.addImport("core", core);
    check.addImport("cockpit_engine", createTsEngine(
        b,
        target,
        optimize,
        sdk_module,
        measure,
        false,
        null,
    ));
    const compiled = b.addTest(.{
        .name = "disabled-phux-provider",
        .root_module = check,
    });
    test_step.dependOn(&compiled.step);
}

/// The native engine's unit and regression tests, rooted at a test-only facade
/// with no `main`.
fn addNativeRegressionTests(
    b: *std.Build,
    artifacts: native_sdk.AppArtifacts,
    test_step: *std.Build.Step,
    measure: bool,
    phux_enabled: bool,
    ffi: ?PhuxFfi,
) void {
    const app_test_root = artifacts.tests.root_module;
    const target = app_test_root.resolved_target.?;
    const optimize = app_test_root.optimize.?;
    const sdk_module = app_test_root.import_table.get("native_sdk") orelse
        @panic("native-sdk app graph did not expose its root module");
    const root = b.createModule(.{
        .root_source_file = b.path("src/native_test_root.zig"),
        .target = target,
        .optimize = optimize,
    });
    root.addImport("native_sdk", sdk_module);

    const contract = b.createModule(.{
        .root_source_file = b.path("src/providers/contract.zig"),
        .target = target,
        .optimize = optimize,
    });
    contract.addImport("native_sdk", sdk_module);
    root.addImport("provider_contract", contract);

    const phux_options = b.addOptions();
    phux_options.addOption(bool, "enabled", phux_enabled);
    root.addImport("phux_options", phux_options.createModule());
    const test_options = b.addOptions();
    test_options.addOption(bool, "measure", measure);
    root.addImport("test_options", test_options.createModule());

    const ghostty = b.dependency("ghostty", .{
        .target = target,
        .optimize = optimize,
        .simd = false,
        .@"emit-xcframework" = false,
        .@"emit-macos-app" = false,
    });
    root.addImport("ghostty-vt", ghostty.module("ghostty-vt"));
    if (phux_enabled) {
        const modules = createPhuxModules(b, target, optimize, sdk_module, contract, ffi.?);
        attachPhuxModules(b, root, modules);
    }

    const credential_store = b.createModule(.{
        .root_source_file = b.path("src/security/credential_store.zig"),
        .target = target,
        .optimize = optimize,
    });
    const provider_identity = b.createModule(.{
        .root_source_file = b.path("src/security/provider_identity.zig"),
        .target = target,
        .optimize = optimize,
    });
    const credential_trust = b.createModule(.{
        .root_source_file = b.path("src/security/trust.zig"),
        .target = target,
        .optimize = optimize,
    });
    root.addImport("credential_store", credential_store);
    root.addImport("provider_identity", provider_identity);
    root.addImport("credential_trust", credential_trust);
    if (target.result.os.tag == .macos) {
        const macos_keychain = b.createModule(.{
            .root_source_file = b.path("src/security/macos_keychain.zig"),
            .target = target,
            .optimize = optimize,
        });
        macos_keychain.addImport("credential_store", credential_store);
        if (b.sysroot) |sysroot| macos_keychain.addFrameworkPath(.{
            .cwd_relative = b.pathJoin(&.{ sysroot, "System/Library/Frameworks" }),
        });
        macos_keychain.linkFramework("Security", .{});
        macos_keychain.linkFramework("CoreFoundation", .{});
        root.addImport("macos_keychain", macos_keychain);
    }

    const tests = b.addTest(.{ .name = "cockpit-native-engine-regressions", .root_module = root });
    if (phux_enabled) keepRingP256Helpers(tests);
    test_step.dependOn(&b.addRunArtifact(tests).step);
}

// ---------------------------------------------------------------- phux FFI

/// ring's Apple ARM64 P-256 wrappers `bl` file-local helpers that dead_strip
/// removes (leaving `udf #0`), which SIGILLs QUIC TLS in `phux-remote-tunnel`.
/// Keep the helpers.
fn keepRingP256Helpers(compile: *std.Build.Step.Compile) void {
    compile.link_gc_sections = false;
}

/// A validated location for the phux client FFI: a directory holding
/// `phux/client.h` and a directory holding `libphux_client_ffi.a`.
const PhuxFfi = struct {
    include_dir: []const u8,
    lib_dir: []const u8,
    /// Human-readable provenance, printed in the test verdict so the reader
    /// knows which lookup won.
    origin: []const u8,
};

fn rootPath(b: *std.Build, sub_path: []const u8) []const u8 {
    const root = b.build_root.path orelse ".";
    return b.pathJoin(&.{ root, sub_path });
}

/// `path` may be absolute or relative to the build root; Io.Dir.access resolves
/// a relative sub-path against the directory handle and ignores it for an
/// absolute one.
fn fileExists(b: *std.Build, path: []const u8) bool {
    b.build_root.handle.access(b.graph.io, path, .{}) catch return false;
    return true;
}

/// The two files scripts/package-macos.sh refuses to package without
/// (scripts/package-macos.sh:81 and :85). Same check, same failure mode, so a
/// location that satisfies the test graph also satisfies packaging.
fn ffiComplete(b: *std.Build, include_dir: []const u8, lib_dir: []const u8) bool {
    return fileExists(b, b.pathJoin(&.{ include_dir, "phux", "client.h" })) and
        fileExists(b, b.pathJoin(&.{ lib_dir, "libphux_client_ffi.a" }));
}

/// Where the phux client FFI can be, in precedence order; each candidate is
/// validated by ffiComplete:
///   1. -Dphux-client-ffi-include-dir / -Dphux-client-ffi-lib-dir
///   2. $PHUX_CLIENT_FFI_INCLUDE_DIR / $PHUX_CLIENT_FFI_LIB_DIR
///   3. ../../target/<profile> (ffi-release by default)
fn resolvePhuxFfi(
    b: *std.Build,
    opt_include: ?[]const u8,
    opt_lib: ?[]const u8,
    ffi_profile: []const u8,
) ?PhuxFfi {
    if (opt_include) |include_dir| {
        if (opt_lib) |lib_dir| {
            if (ffiComplete(b, include_dir, lib_dir)) return .{
                .include_dir = include_dir,
                .lib_dir = lib_dir,
                .origin = "-Dphux-client-ffi-include-dir / -Dphux-client-ffi-lib-dir",
            };
        }
    }

    if (b.graph.environ_map.get("PHUX_CLIENT_FFI_INCLUDE_DIR")) |include_dir| {
        if (b.graph.environ_map.get("PHUX_CLIENT_FFI_LIB_DIR")) |lib_dir| {
            if (ffiComplete(b, include_dir, lib_dir)) return .{
                .include_dir = include_dir,
                .lib_dir = lib_dir,
                .origin = "$PHUX_CLIENT_FFI_INCLUDE_DIR / $PHUX_CLIENT_FFI_LIB_DIR",
            };
        }
    }

    const include_dir = rootPath(b, "../../crates/phux-client-ffi/include");
    const lib_dir = rootPath(b, b.pathJoin(&.{ "../..", "target", ffi_profile }));
    if (ffiComplete(b, include_dir, lib_dir)) return .{
        .include_dir = include_dir,
        .lib_dir = lib_dir,
        .origin = b.fmt("Phux monorepo checkout ({s})", .{ffi_profile}),
    };

    return null;
}

// ------------------------------------------------------------ phux modules

const PhuxModules = struct {
    transport: *std.Build.Module,
    extension: *std.Build.Module,
    host: *std.Build.Module,
    provider: *std.Build.Module,
    pointer: *std.Build.Module,
};

/// The whole of src/providers/phux/, wired the same way whether it is going
/// into the app graph or into standalone test artifacts. One definition, so
/// the graph the tests compile cannot drift from the graph the app ships.
fn createPhuxModules(
    b: *std.Build,
    target: std.Build.ResolvedTarget,
    optimize: std.builtin.OptimizeMode,
    sdk_module: *std.Build.Module,
    provider_contract: *std.Build.Module,
    ffi: PhuxFfi,
) PhuxModules {
    // ref.zig is shared by several modules, so it must be a module itself;
    // a relative @import from two modules is rejected by Zig.
    const ref_module = b.createModule(.{
        .root_source_file = b.path("src/providers/phux/ref.zig"),
        .target = target,
        .optimize = optimize,
    });

    const transport_module = b.createModule(.{
        .root_source_file = b.path("src/providers/phux/transport.zig"),
        .target = target,
        .optimize = optimize,
    });
    transport_module.addImport("phux_ref", ref_module);
    transport_module.link_libc = true;
    const extension_module = b.createModule(.{
        .root_source_file = b.path("src/providers/phux/extension.zig"),
        .target = target,
        .optimize = optimize,
    });
    extension_module.addImport("native_sdk", sdk_module);
    extension_module.addImport("phux_transport", transport_module);
    // The socket worker drives phux-client-ffi's remote-host tunnel
    // (remote_tunnel.zig), and its tests are rooted without the host module,
    // so it carries the header and archive itself.
    extension_module.addIncludePath(.{ .cwd_relative = ffi.include_dir });
    extension_module.addObjectFile(.{
        .cwd_relative = b.pathJoin(&.{ ffi.lib_dir, "libphux_client_ffi.a" }),
    });
    extension_module.linkSystemLibrary("c", .{});

    const host_module = b.createModule(.{
        .root_source_file = b.path("src/providers/phux/host.zig"),
        .target = target,
        .optimize = optimize,
    });
    host_module.addImport("provider_contract", provider_contract);
    host_module.addImport("native_sdk", sdk_module);
    host_module.addImport("phux_transport", transport_module);
    host_module.addImport("phux_ref", ref_module);
    host_module.addIncludePath(.{ .cwd_relative = ffi.include_dir });
    host_module.addObjectFile(.{
        .cwd_relative = b.pathJoin(&.{ ffi.lib_dir, "libphux_client_ffi.a" }),
    });
    host_module.linkSystemLibrary("c", .{});
    // spike: Rust's std unwinder on linux-gnu.
    if (target.result.os.tag == .linux) for ([_]*std.Build.Module{ host_module, extension_module }) |module| module.linkSystemLibrary("gcc_s", .{});
    // phux-config's time zone lookup needs CoreFoundation in the standalone
    // phux test artifacts.
    if (target.result.os.tag == .macos) {
        for ([_]*std.Build.Module{ host_module, extension_module }) |module| {
            if (b.sysroot) |sysroot| module.addFrameworkPath(.{
                .cwd_relative = b.pathJoin(&.{ sysroot, "System/Library/Frameworks" }),
            });
            module.linkFramework("CoreFoundation", .{});
        }
    }

    const provider_module = b.createModule(.{
        .root_source_file = b.path("src/providers/phux/provider.zig"),
        .target = target,
        .optimize = optimize,
    });
    provider_module.addImport("native_sdk", sdk_module);
    provider_module.addImport("provider_contract", provider_contract);
    provider_module.addImport("phux_host", host_module);
    provider_module.addImport("phux_transport", transport_module);
    provider_module.addImport("phux_extension", extension_module);
    provider_module.addImport("phux_ref", ref_module);

    const pointer_module = b.createModule(.{
        .root_source_file = b.path("src/providers/phux/pointer.zig"),
        .target = target,
        .optimize = optimize,
    });
    pointer_module.addImport("native_sdk", sdk_module);
    pointer_module.addImport("phux_ref", ref_module);

    return .{
        .transport = transport_module,
        .extension = extension_module,
        .host = host_module,
        .provider = provider_module,
        .pointer = pointer_module,
    };
}

fn attachPhuxModules(b: *std.Build, root: *std.Build.Module, modules: PhuxModules) void {
    root.addImport("phux_provider", modules.provider);
    root.addImport("phux_pointer", modules.pointer);
    root.linkSystemLibrary("c", .{});
    if (root.resolved_target.?.result.os.tag != .macos) return;
    root.addCSourceFile(.{
        .file = b.path("src/providers/phux/pointer_macos.m"),
        .flags = &.{ "-fobjc-arc", "-fblocks" },
    });
    if (b.sysroot) |sysroot| {
        root.addFrameworkPath(.{
            .cwd_relative = b.pathJoin(&.{ sysroot, "System/Library/Frameworks" }),
        });
    }
    root.linkFramework("AppKit", .{});
}

/// The phux modules whose own tests `test` runs, reported verbatim in the
/// verdict.
const phux_test_module_names = "transport, host, provider, pointer, extension";

/// Root a test artifact at each phux module so its tests run as part of
/// `zig build test` regardless of -Dphux-enabled (Zig only runs tests from a
/// compilation's root module).
fn addPhuxGraphTests(
    b: *std.Build,
    artifacts: native_sdk.AppArtifacts,
    test_step: *std.Build.Step,
    ffi: PhuxFfi,
) void {
    // Built like the rest of `test` (not the shipping exe root), keeping
    // safety checks.
    const root = artifacts.tests.root_module;
    const target = root.resolved_target.?;
    const optimize = root.optimize.?;
    const sdk_module = root.import_table.get("native_sdk") orelse
        @panic("native-sdk app graph did not expose its root module");

    // A contract module of its own: the app graph's copy belongs to the app
    // graph, and these artifacts must not depend on the app being built.
    const provider_contract = b.createModule(.{
        .root_source_file = b.path("src/providers/contract.zig"),
        .target = target,
        .optimize = optimize,
    });
    provider_contract.addImport("native_sdk", sdk_module);

    const modules = createPhuxModules(b, target, optimize, sdk_module, provider_contract, ffi);
    @import("tests/everyday-remote/build.zig").add(b, modules.provider);

    // pointer.zig declares phux_pointer_monitor_start/stop, which live in
    // pointer_macos.m. The app graph adds that source to its own root; a
    // standalone test artifact has to carry it.
    if (target.result.os.tag == .macos) {
        modules.pointer.addCSourceFile(.{
            .file = b.path("src/providers/phux/pointer_macos.m"),
            .flags = &.{ "-fobjc-arc", "-fblocks" },
        });
        if (b.sysroot) |sysroot| {
            modules.pointer.addFrameworkPath(.{
                .cwd_relative = b.pathJoin(&.{ sysroot, "System/Library/Frameworks" }),
            });
        }
        modules.pointer.linkFramework("AppKit", .{});
    }
    modules.pointer.linkSystemLibrary("c", .{});

    // Keep this in step with phux_test_module_names.
    const rooted = [_]struct { name: []const u8, module: *std.Build.Module }{
        .{ .name = "phux-transport-tests", .module = modules.transport },
        .{ .name = "phux-host-tests", .module = modules.host },
        .{ .name = "phux-provider-tests", .module = modules.provider },
        .{ .name = "phux-pointer-tests", .module = modules.pointer },
        .{ .name = "phux-extension-tests", .module = modules.extension },
    };
    for (rooted) |entry| {
        const tests = b.addTest(.{ .name = entry.name, .root_module = entry.module });
        keepRingP256Helpers(tests);
        test_step.dependOn(&b.addRunArtifact(tests).step);
    }
}

// --------------------------------------------------------------- verdict

/// Print, last, what `zig build test` compiled. It runs only if every other
/// step succeeded, so no verdict means not green. stdio is inherited because
/// captured stderr makes Zig report the step as failed.
fn addTestVerdict(b: *std.Build, test_step: *std.Build.Step, verdict: []const u8) void {
    const previous = b.allocator.dupe(*std.Build.Step, test_step.dependencies.items) catch @panic("OOM");
    const run = b.addSystemCommand(&.{ "/usr/bin/printf", "%s\n", verdict });
    run.stdio = .inherit;
    run.has_side_effects = true; // never cached away; the verdict must print every run
    for (previous) |dependency| run.step.dependOn(dependency);
    test_step.dependOn(&run.step);
}

/// Which Zig global cache this run used. A shared cache's manifest locks can
/// starve other worktrees; `scripts/zig-build.sh` isolates it, and this line
/// makes the exposure visible in every log.
fn globalCacheNote(b: *std.Build, source_root: []const u8) []const u8 {
    const reported = b.graph.global_cache_root.path orelse return "(unknown)";

    // Zig reports a cache under the build root as a relative path.
    const absolute = if (std.fs.path.isAbsolute(reported)) reported else b.pathFromRoot(reported);
    if (std.mem.startsWith(u8, absolute, source_root)) return b.fmt("{s} (worktree-private)", .{absolute});
    const path = absolute;
    return b.fmt("{s}\n                 SHARED with every other checkout on this machine; one\n                 stuck build runner in any of them can starve this one.\n                 Use scripts/zig-build.sh to get a private cache.", .{path});
}

fn buildVerdict(
    b: *std.Build,
    phux_enabled: bool,
    ffi: ?PhuxFfi,
    ffi_profile: []const u8,
) []const u8 {
    const rule = "------------------------------------------------------------------";

    // Print the build root so a result from the wrong worktree is visible.
    const source_root = b.build_root.path orelse ".";
    const global_cache = globalCacheNote(b, source_root);

    if (ffi) |found| {
        return b.fmt(
            \\{s}
            \\zig build test: PASS
            \\  source root:   {s}
            \\  global cache:  {s}
            \\  phux provider: COMPILED AND TESTED ({s})
            \\    ffi include: {s}
            \\    ffi lib:     {s}
            \\    found via:   {s}
            \\  app graph:     {s}
            \\{s}
        , .{
            rule,
            source_root,
            global_cache,
            phux_test_module_names,
            found.include_dir,
            found.lib_dir,
            found.origin,
            if (phux_enabled)
                "phux provider (-Dphux-enabled=true); disabled provider compiled (disabled-phux-provider)"
            else
                "local terminal provider (-Dphux-enabled defaults to false)",
            rule,
        });
    }
    return b.fmt(
        \\{s}
        \\zig build test: PASS, INCOMPLETE
        \\  source root:   {s}
        \\  global cache:  {s}
        \\  phux provider: NOT COMPILED. src/providers/phux/ was not in this
        \\                 build at all, so a change to it is NOT verified by
        \\                 this run, however green it looks.
        \\  reason:        the phux client FFI was not found. Looked for
        \\                 phux/client.h and libphux_client_ffi.a under, in order:
        \\                   -Dphux-client-ffi-include-dir / -Dphux-client-ffi-lib-dir
        \\                   $PHUX_CLIENT_FFI_INCLUDE_DIR / $PHUX_CLIENT_FFI_LIB_DIR
        \\                   {s}
        \\  to include it: cargo build --locked --profile {s} \
        \\                   -p phux-client-ffi --manifest-path ../../Cargo.toml
        \\                 then re-run zig build test -Dphux-client-ffi-profile={s}
        \\  app graph:     local terminal provider (-Dphux-enabled defaults to false)
        \\{s}
    , .{
        rule,
        source_root,
        global_cache,
        rootPath(b, "../.."),
        ffi_profile,
        ffi_profile,
        rule,
    });
}

// ----------------------------------------------------------------- build

/// All app entry points ship the same-checkout CLI next to the executable. The
/// package copy runs after the SDK assembles its skeleton; test builds do not
/// build or run the CLI. Cargo provides incremental source freshness.
fn addCoordinatorCli(b: *std.Build, artifacts: native_sdk.AppArtifacts, profile: []const u8) void {
    const cli_path = b.getInstallPath(.bin, "phux");
    const cli = b.addSystemCommand(&.{ "bash", rootPath(b, "scripts/build-phux-cli.sh"), profile, cli_path });
    cli.has_side_effects = true;
    b.getInstallStep().dependOn(&cli.step);
    // The SDK's default run points into the compiler cache. Run the installed
    // pair instead, where runtime sibling discovery is identical to the bundle.
    artifacts.run.argv.items[0] = .{ .bytes = b.dupe(b.getInstallPath(.bin, "phux-cockpit")) };
    artifacts.run.step.dependOn(&artifacts.install.step);
    artifacts.run.step.dependOn(&cli.step);
    if (b.top_level_steps.get("package")) |package| {
        const copy = b.addSystemCommand(&.{ "bash", rootPath(b, "scripts/stage-phux-cli.sh"), cli_path, rootPath(b, "zig-out/package/phux-cockpit.app/Contents/MacOS/phux") });
        copy.has_side_effects = true;
        for (package.step.dependencies.items) |dependency| copy.step.dependOn(dependency);
        copy.step.dependOn(&cli.step);
        // Sign only after all nested executables have been staged. Dev packages
        // must be runnable too; release packaging replaces this
        // ad-hoc signature with its configured identity after adding resources.
        const app_path = rootPath(b, "zig-out/package/phux-cockpit.app");
        const sign = b.addSystemCommand(&.{ "/usr/bin/codesign", "--force", "--deep", "--timestamp=none", "--sign", "-", app_path });
        sign.has_side_effects = true;
        sign.step.dependOn(&copy.step);
        const verify = b.addSystemCommand(&.{ "/usr/bin/codesign", "--verify", "--deep", "--strict", app_path });
        verify.has_side_effects = true;
        verify.step.dependOn(&sign.step);
        package.step.dependOn(&verify.step);
    }
}

pub fn build(b: *std.Build) void {
    const dependency = b.dependency("native_sdk", .{});
    const phux_enabled = b.option(
        bool,
        "phux-enabled",
        "Build the production Phux provider instead of the local terminal provider",
    ) orelse false;
    const opt_include = b.option(
        []const u8,
        "phux-client-ffi-include-dir",
        "Directory containing phux/client.h (required with -Dphux-enabled=true)",
    );
    const opt_lib = b.option(
        []const u8,
        "phux-client-ffi-lib-dir",
        "Directory containing libphux_client_ffi.a (required with -Dphux-enabled=true)",
    );
    const ffi_profile = b.option(
        []const u8,
        "phux-client-ffi-profile",
        "Cargo target profile directory for monorepo FFI lookup (ffi-dev for iteration)",
    ) orelse "ffi-release";
    const measure = b.option(
        bool,
        "measure",
        "Print MEASURED diagnostics from tests (see src/tests/measured.zig)",
    ) orelse false;
    const ffi = resolvePhuxFfi(b, opt_include, opt_lib, ffi_profile);

    // -Dphux-enabled=true is a promise that the selected app graph contains
    // the real provider. Never silently downgrade either composition root.
    if (phux_enabled and ffi == null) {
        std.log.err(
            \\-Dphux-enabled=true, but the phux client FFI was not found.
            \\Both of these must exist:
            \\  <include-dir>/phux/client.h
            \\  <lib-dir>/libphux_client_ffi.a
            \\Pass -Dphux-client-ffi-include-dir=<dir> -Dphux-client-ffi-lib-dir=<dir>,
            \\or set PHUX_CLIENT_FFI_INCLUDE_DIR and PHUX_CLIENT_FFI_LIB_DIR,
            \\or build the FFI from the Phux monorepo root at {s} with:
            \\  cargo build --locked --profile {s} -p phux-client-ffi
        , .{ rootPath(b, "../.."), ffi_profile });
        std.process.exit(1);
    }
    // Before the SDK's own configure-time check inside addAppArtifacts,
    // which exits the build when the compiler is missing: the installer has
    // to have had its turn first, or it never gets one (CI proved that).
    ensureTsToolchain(b, dependency);
    const artifacts = native_sdk.addAppArtifacts(b, dependency, .{
        .name = "phux-cockpit",
        .native_extension = "src/native_extension.zig",
    });
    keepRingP256Helpers(artifacts.exe);
    addTsEngineModules(b, artifacts, measure, phux_enabled, ffi);
    if (phux_enabled) addCoordinatorCli(b, artifacts, ffi_profile);

    if (b.top_level_steps.get("test")) |top_level| {
        const test_step = &top_level.step;
        addNativeRegressionTests(b, artifacts, test_step, measure, phux_enabled, ffi);
        if (ffi) |found| addPhuxGraphTests(b, artifacts, test_step, found);
        if (phux_enabled) addDisabledProviderCompileCheck(b, artifacts, test_step, measure);
        addTestVerdict(b, test_step, buildVerdict(b, phux_enabled, ffi, ffi_profile));
    }
}
