// The local Cockpit remains the default APP graph. The production Phux lane is
// materialized into the app under -Dphux-enabled=true, so default `zig build`
// and `zig build run` never resolve its C header, static archive, Objective-C
// source, or AppKit, and never swap the local terminal provider out.
//
// `zig build test` is deliberately different. Independently of -Dphux-enabled,
// it compiles and runs the phux provider's own modules whenever the phux client
// FFI can be found on this machine (see resolvePhuxFfi). Before that, a
// signature change to PhuxProvider.search and Host.search passed a local
// `zig build test` cleanly while never being compiled at all, because
// -Dphux-enabled defaults to false. CI caught it later; the local run had
// reported a false green.
//
// Whatever the outcome of that lookup, the test step ends by printing a verdict
// naming exactly what was compiled -- see addTestVerdict. The exit code is
// still the only authority on pass/fail; the verdict answers "was the run
// real", which the exit code cannot.
const std = @import("std");
const native_sdk = @import("native_sdk");

fn addImportToArtifacts(
    artifacts: native_sdk.AppArtifacts,
    name: []const u8,
    module: *std.Build.Module,
) void {
    artifacts.exe.root_module.addImport(name, module);
    if (artifacts.tests.root_module != artifacts.exe.root_module)
        artifacts.tests.root_module.addImport(name, module);
}

fn addProviderContractModules(
    b: *std.Build,
    artifacts: native_sdk.AppArtifacts,
) void {
    const roots = [_]*std.Build.Module{
        artifacts.exe.root_module,
        artifacts.tests.root_module,
    };
    for (roots, 0..) |root, index| {
        if (index == 1 and root == artifacts.exe.root_module) continue;
        const contract = b.createModule(.{
            .root_source_file = b.path("src/providers/contract.zig"),
            .target = root.resolved_target.?,
            .optimize = root.optimize.?,
        });
        const sdk_module = root.import_table.get("native_sdk") orelse
            @panic("native-sdk app graph did not expose its root module");
        contract.addImport("native_sdk", sdk_module);
        root.addImport("provider_contract", contract);
    }
}

fn addSecurityModules(
    b: *std.Build,
    artifacts: native_sdk.AppArtifacts,
) void {
    const roots = [_]*std.Build.Module{
        artifacts.exe.root_module,
        artifacts.tests.root_module,
    };
    for (roots, 0..) |root, index| {
        if (index == 1 and root == artifacts.exe.root_module) continue;
        const target = root.resolved_target.?;
        const optimize = root.optimize.?;
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
            if (b.sysroot) |sysroot| {
                macos_keychain.addFrameworkPath(.{
                    .cwd_relative = b.pathJoin(&.{ sysroot, "System/Library/Frameworks" }),
                });
            }
            macos_keychain.linkFramework("Security", .{});
            macos_keychain.linkFramework("CoreFoundation", .{});
            root.addImport("macos_keychain", macos_keychain);
            if (root == artifacts.tests.root_module) {
                if (b.top_level_steps.get("test")) |top_level| {
                    const tests = b.addTest(.{
                        .name = "macos-keychain-tests",
                        .root_module = macos_keychain,
                    });
                    top_level.step.dependOn(&b.addRunArtifact(tests).step);
                }
            }
        }
    }
}

/// The TypeScript-core graph: the runner extension under `typescript-spike/`
/// fronts a real Cockpit engine, and a Zig module may only import files
/// below its own root, so the engine is handed to each extension instance
/// (exe, app tests, extension tests) as a module rooted at `src/ts_engine.zig`
/// carrying the same imports the Zig app root gets, including the production
/// Phux provider when selected.
/// The SDK package's TypeScript frontend needs its compiler (`scriptc`) under
/// packages/core/node_modules, and the tarball pin does not carry it: every
/// pin bump used to stop at "cannot resolve its TypeScript toolchain" until
/// someone ran `npm ci` in a cache directory by hand. Install it once, at
/// configure time, for the package this build actually resolved. The default
/// and Phux graphs never reach this.
fn ensureTsToolchain(b: *std.Build, dependency: *std.Build.Dependency) void {
    const root = dependency.builder.build_root.path orelse return;
    const core_dir = b.pathJoin(&.{ root, "packages", "core" });
    const probe = b.pathJoin(&.{ core_dir, "node_modules", "scriptc", "dist", "main.js" });
    if (std.Io.Dir.cwd().access(b.graph.io, probe, .{})) |_| {
        return;
    } else |_| {}
    std.debug.print("typescript-spike: installing the SDK package's TypeScript toolchain once in {s}\n", .{core_dir});
    const result = std.process.run(b.allocator, b.graph.io, .{
        .argv = &.{ "npm", "ci", "--include=dev" },
        .cwd = .{ .path = core_dir },
    }) catch |err| {
        std.debug.print("typescript-spike: could not run npm ci in {s}: {s}. Install Node 24+ and run it by hand.\n", .{ core_dir, @errorName(err) });
        return;
    };
    switch (result.term) {
        .exited => |code| if (code == 0) return,
        else => {},
    }
    std.debug.print("typescript-spike: npm ci failed in {s}:\n{s}\n", .{ core_dir, result.stderr });
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
        root.addImport("cockpit_engine", engine);
    }
}

// ---------------------------------------------------------------- phux FFI

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

/// Where the phux client FFI can be, in precedence order. Every entry is
/// validated by ffiComplete before it is accepted, so a half-present checkout
/// is treated as absent rather than exploding mid-build.
///
///   1. -Dphux-client-ffi-include-dir / -Dphux-client-ffi-lib-dir
///   2. $PHUX_CLIENT_FFI_INCLUDE_DIR / $PHUX_CLIENT_FFI_LIB_DIR
///      (the pair scripts/package-macos.sh and both workflows already use)
///   3. ../../...           -- the Phux monorepo root
fn resolvePhuxFfi(
    b: *std.Build,
    opt_include: ?[]const u8,
    opt_lib: ?[]const u8,
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

    const layouts = [_]struct { root: []const u8, origin: []const u8 }{
        .{ .root = "../..", .origin = "Phux monorepo checkout" },
    };
    for (layouts) |layout| {
        const include_dir = rootPath(b, b.pathJoin(&.{ layout.root, "crates/phux-client-ffi/include" }));
        const lib_dir = rootPath(b, b.pathJoin(&.{ layout.root, "target/ffi-release" }));
        if (ffiComplete(b, include_dir, lib_dir)) return .{
            .include_dir = include_dir,
            .lib_dir = lib_dir,
            .origin = layout.origin,
        };
    }

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
    // ref.zig is shared by every module below and must be a MODULE, not a
    // relative @import. These files are compiled into several artifacts at
    // once, and a plain `@import("ref.zig")` from two of them puts the same
    // file in two modules, which Zig rejects with
    // "file exists in modules 'root' and 'phux_transport'".
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
    const extension_module = b.createModule(.{
        .root_source_file = b.path("src/providers/phux/extension.zig"),
        .target = target,
        .optimize = optimize,
    });
    extension_module.addImport("native_sdk", sdk_module);
    extension_module.addImport("phux_transport", transport_module);

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

fn addPhuxModules(
    b: *std.Build,
    artifacts: native_sdk.AppArtifacts,
    ffi: PhuxFfi,
) void {
    const roots = [_]*std.Build.Module{
        artifacts.exe.root_module,
        artifacts.tests.root_module,
    };
    for (roots, 0..) |root, index| {
        if (index == 1 and root == artifacts.exe.root_module) continue;
        const sdk_module = root.import_table.get("native_sdk") orelse
            @panic("native-sdk app graph did not expose its root module");
        const provider_contract = root.import_table.get("provider_contract") orelse
            @panic("cockpit graph did not expose its provider contract module");

        const modules = createPhuxModules(
            b,
            root.resolved_target.?,
            root.optimize.?,
            sdk_module,
            provider_contract,
            ffi,
        );

        attachPhuxModules(b, root, modules);
    }
}

fn attachPhuxModules(b: *std.Build, root: *std.Build.Module, modules: PhuxModules) void {
    root.addImport("phux_provider", modules.provider);
    root.addImport("phux_pointer", modules.pointer);
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
    root.linkSystemLibrary("c", .{});
}

/// Names of the phux modules whose own tests the `test` step runs, in the
/// order they are added. Reported verbatim in the test verdict, so the verdict
/// cannot claim more coverage than this list delivers.
///
/// extension.zig is now rooted too. It was held out because rooting it hung the
/// build indefinitely inside extension.writeExact: that function's 1s deadline
/// is only evaluated between send() calls, and macOS ignores MSG_DONTWAIT on an
/// AF_UNIX socket that is blocking at the descriptor level, so send() slept
/// forever and the deadline was unreachable (phux-cockpit-iwf). writeExact now
/// forces O_NONBLOCK for the duration of the write -- see the derivation in
/// scripts/measure-send-blocking.c -- so the deadline is enforceable and these
/// six tests, which had never executed anywhere including CI, run every build.
const phux_test_module_names = "transport, host, provider, pointer, extension";

/// Compile src/providers/phux/ and run its tests as part of `zig build test`,
/// regardless of -Dphux-enabled.
///
/// Zig runs tests only from a compilation's ROOT module, so importing these
/// modules into the app graph -- which is all -Dphux-enabled=true did -- type
/// checks them but runs only host.zig's tests. Rooting a test artifact at each
/// module both compiles the whole provider and runs the tests in transport.zig,
/// provider.zig and pointer.zig, which never ran anywhere, including CI.
fn addPhuxGraphTests(
    b: *std.Build,
    artifacts: native_sdk.AppArtifacts,
    test_step: *std.Build.Step,
    ffi: PhuxFfi,
) void {
    // The TEST root, not the exe root: these artifacts must be built the way
    // the rest of `zig build test` is built. The exe root is optimized for
    // shipping, and compiling tests that way silently drops the safety checks
    // the assertions are relying on.
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

    // pointer.zig declares phux_pointer_monitor_start/stop, which live in
    // pointer_macos.m. The app graph adds that source to its own root; a
    // standalone test artifact has to carry it.
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
        test_step.dependOn(&b.addRunArtifact(tests).step);
    }
}

// --------------------------------------------------------------- verdict

/// Print, as the last thing `zig build test` does, an unambiguous statement of
/// (a) that the run passed and (b) what was actually compiled.
///
/// This step is wired to depend on every step `test` already depended on, and
/// `test` is then made to depend on it. So it runs last, and it runs ONLY if
/// everything before it succeeded -- which is what licenses it to print PASS.
/// The corollary is the important half: no verdict line means the run was not
/// green. The process exit code remains the authority either way.
///
/// stdio is .inherit on purpose. A run step's captured output lands in
/// `Step.result_stderr`, and Zig 0.16's build runner prints a step-failure
/// report -- including `failed command: ...` -- for any step with non-empty
/// result_stderr, pass or fail (build_runner.zig:1381). Capturing the verdict
/// would make the verdict itself look like a failure.
fn addTestVerdict(b: *std.Build, test_step: *std.Build.Step, verdict: []const u8) void {
    const previous = b.allocator.dupe(*std.Build.Step, test_step.dependencies.items) catch @panic("OOM");
    const run = b.addSystemCommand(&.{ "/usr/bin/printf", "%s\n", verdict });
    run.stdio = .inherit;
    run.has_side_effects = true; // never cached away; the verdict must print every run
    for (previous) |dependency| run.step.dependOn(dependency);
    test_step.dependOn(&run.step);
}

/// Which Zig global cache this run used, and whether anybody else is in it.
///
/// The other half of phux-cockpit-2ml.11. A cache entry is guarded by an
/// EXCLUSIVE lock on its manifest in `<global cache>/h/<hash>.txt`, held for as
/// long as the entry takes to produce, and the manifests that land there are
/// project-independent -- a hello-world and this repo were measured writing the
/// same one. So with the default `~/.cache/zig`, every worktree on the machine
/// queues on the same file, and a build runner that outlives its session holds
/// it forever. Measured: a deliberately held lock blocked a shared-cache build
/// past 45s while an isolated-cache build finished in 3s.
///
/// This line does not prevent that -- `scripts/zig-build.sh` does, by giving
/// each worktree its own cache with only the package directory shared. What
/// this line does is make the exposure legible in the log of every run,
/// including the ones that call `zig build` directly.
fn globalCacheNote(b: *std.Build, source_root: []const u8) []const u8 {
    const reported = b.graph.global_cache_root.path orelse return "(unknown)";

    // Zig hands this back RELATIVE to the build root whenever the cache lives
    // under it -- `--global-cache-dir <abs path>/.zig-global-cache` came back
    // as plain `.zig-global-cache`. The first version of this function compared
    // the reported string against the absolute source root, so the one
    // configuration it was written to certify -- a worktree-private cache --
    // was the one it called SHARED. Caught by reading the output of the first
    // run that used it, which is the only reason it is not still wrong.
    const absolute = if (std.fs.path.isAbsolute(reported)) reported else b.pathFromRoot(reported);
    if (std.mem.startsWith(u8, absolute, source_root)) return b.fmt("{s} (worktree-private)", .{absolute});
    const path = absolute;
    return b.fmt("{s}\n                 SHARED with every other checkout on this machine; one\n                 stuck build runner in any of them can starve this one.\n                 Use scripts/zig-build.sh to get a private cache.", .{path});
}

// ----------------------------------------------------------------- guards

/// Run `scripts/guard-check.sh` as part of `zig build test`.
///
/// A GUARD is a regression test that has been watched failing with its fix
/// removed, and `scripts/guards/*.guard` holds the break that did it. This
/// step is the CHEAP half of that ritual: it never builds or runs anything,
/// it only checks that the bookkeeping is still true -- every marker has a
/// guard file, every guard file names a test that still exists, every guard
/// has been demonstrated red, and every recorded break still applies to the
/// tree it claims to break.
///
/// It belongs in the gate because the expensive half cannot be. Proving each
/// guard red costs a full test run apiece, which is minutes; this costs
/// milliseconds, and without it the ledger silently rots into a set of claims
/// nobody has checked since the day they were written. That rot is exactly
/// phux-cockpit-2ml.2's subject: a regression test nobody re-derives is a
/// test-shaped comment.
///
/// `stdio = .inherit` and the script's silence-on-success are a pair, for the
/// reason spelled out on `addTestVerdict`: Zig 0.16's build runner prints a
/// step-failure report for any step with non-empty captured stderr, so a
/// chatty check would look like a failing one.
fn addGuardCheck(b: *std.Build, test_step: *std.Build.Step) void {
    const run = b.addSystemCommand(&.{"bash"});
    run.addFileArg(b.path("scripts/guard-check.sh"));
    run.stdio = .inherit;
    run.has_side_effects = true; // the tree it reads is not a declared input
    test_step.dependOn(&run.step);
}

fn buildVerdict(
    b: *std.Build,
    phux_enabled: bool,
    ffi: ?PhuxFfi,
) []const u8 {
    const rule = "------------------------------------------------------------------";

    // Which TREE this exit code describes.
    //
    // Not decoration. On 2026-08-12 a subagent's `zig build test` silently ran
    // in a sibling git worktree instead of its own: the exit code was real, it
    // just described somebody else's code. Nothing in the output distinguished
    // it, and it surfaced only because a later run happened to starve on the
    // shared Zig cache lock. That is the same failure class this whole verdict
    // exists for — a green result that does not describe what you changed —
    // and it is the harder one to spot, because a correct-looking PASS from
    // the wrong tree is identical to a correct one. Printing the resolved
    // build root costs a line and makes it visible in every future log.
    // See phux-cockpit-2ml.11.
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
                "phux provider (-Dphux-enabled=true)"
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
            \\  to include it: cargo build --locked --profile ffi-release \
            \\                   -p phux-client-ffi --manifest-path ../../Cargo.toml
            \\                 then re-run zig build test
            \\  app graph:     local terminal provider (-Dphux-enabled defaults to false)
            \\{s}
    , .{
        rule,
        source_root,
        global_cache,
        rootPath(b, "../.."),
        rule,
    });
}

// ----------------------------------------------------------------- build

pub fn build(b: *std.Build) void {
    const dependency = b.dependency("native_sdk", .{});
    const typescript_spike = b.option(
        bool,
        "typescript-spike",
        "Build the isolated TypeScript + Native markup Cockpit artifact",
    ) orelse false;
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
    const measure = b.option(
        bool,
        "measure",
        "Print MEASURED diagnostics from tests (see src/tests/measured.zig)",
    ) orelse false;
    const ffi = resolvePhuxFfi(b, opt_include, opt_lib);

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
            \\  cargo build --locked --profile ffi-release -p phux-client-ffi
        , .{ rootPath(b, "../..") });
        std.process.exit(1);
    }
    // Before the SDK's own configure-time check inside addAppArtifacts,
    // which exits the build when the compiler is missing: the installer has
    // to have had its turn first, or it never gets one (CI proved that).
    if (typescript_spike) ensureTsToolchain(b, dependency);
    const artifacts = if (typescript_spike)
        native_sdk.addAppArtifacts(b, dependency, .{
            .name = "phux-cockpit-typescript-spike",
            .app_root = "typescript-spike",
            .native_extension = "src/native_extension.zig",
        })
    else
        native_sdk.addAppArtifacts(b, dependency, .{ .name = "phux-cockpit" });
    const app_module = artifacts.exe.root_module;
    if (app_module.resolved_target.?.result.os.tag != .macos)
        @panic("phux-cockpit supports macOS only");
    if (typescript_spike) {
        addTsEngineModules(b, artifacts, measure, phux_enabled, ffi);
        if (b.top_level_steps.get("test")) |top_level| {
            const test_step = &top_level.step;
            if (ffi) |found| addPhuxGraphTests(b, artifacts, test_step, found);
            addGuardCheck(b, test_step);
            addTestVerdict(b, test_step, buildVerdict(b, phux_enabled, ffi));
        }
        return;
    }

    const phux_options = b.addOptions();
    phux_options.addOption(bool, "enabled", phux_enabled);
    addImportToArtifacts(artifacts, "phux_options", phux_options.createModule());

    const test_options = b.addOptions();
    test_options.addOption(bool, "measure", measure);
    addImportToArtifacts(artifacts, "test_options", test_options.createModule());

    addProviderContractModules(b, artifacts);
    addSecurityModules(b, artifacts);
    if (phux_enabled) addPhuxModules(b, artifacts, ffi.?);

    const ghostty = b.dependency("ghostty", .{
        .target = app_module.resolved_target.?,
        .optimize = app_module.optimize.?,
        .simd = false,
        .@"emit-xcframework" = false,
        .@"emit-macos-app" = false,
    });
    const vt = ghostty.module("ghostty-vt");
    addImportToArtifacts(artifacts, "ghostty-vt", vt);

    if (b.top_level_steps.get("test")) |top_level| {
        const test_step = &top_level.step;
        if (ffi) |found| addPhuxGraphTests(b, artifacts, test_step, found);
        addGuardCheck(b, test_step);
        addTestVerdict(b, test_step, buildVerdict(b, phux_enabled, ffi));
    }
}
