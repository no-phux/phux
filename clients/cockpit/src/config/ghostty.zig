//! Adopt the user's Ghostty font and colours as Cockpit's defaults, read the
//! way the desktop client reads them (`clients/desktop/scripts/ghostty-config.ts`
//! and `clients/desktop/src/settings/ghostty.ts`): the same files in the same
//! order, `config-file` includes one level deep, a named `theme` resolved from
//! the same two theme directories, and the same value rules. Parsing is total:
//! unknown keys and malformed values are ignored, and nothing here can fail
//! startup.
//!
//! Adopted: `font-family` (when Cockpit ships that face), `font-size`,
//! `background`, `foreground`, `cursor-color`, `selection-background` and
//! `palette = 0..15`. Not adopted, on purpose: `font-thicken` (Cockpit already
//! inks as heavily as Ghostty's strongest setting, so the knob could only thin
//! it; docs/RENDER_FIDELITY.md section 7), `adjust-cell-*` (the grid has no
//! cell adjustment) and `minimum-contrast` (Cockpit keeps its own floor).

const std = @import("std");
const config = @import("config.zig");

pub const Rgb = config.Rgb;
pub const Inherited = config.Inherited;

/// Bounded, like the rest of the config layer: one file of `max_config_bytes`,
/// and at most this many `config-file` includes.
pub const max_includes: usize = 8;
const max_theme_name_bytes: usize = 256;

pub const Path = config.Text(std.fs.max_path_bytes);

/// Where to look, copied out of the environment at startup so a Settings
/// reload re-reads exactly the same places with no ambient state.
pub const Locator = struct {
    pub const Mode = enum { disabled, search, explicit };

    mode: Mode = .disabled,
    home: Path = .{},
    /// `$XDG_CONFIG_HOME`, or `~/.config` when that is unset.
    config_home: Path = .{},
    /// `PHUX_COCKPIT_GHOSTTY_CONFIG` when it names a file.
    explicit: Path = .{},

    /// `override` is `PHUX_COCKPIT_GHOSTTY_CONFIG`: unset searches Ghostty's
    /// own locations, a path reads that file instead, and an empty value turns
    /// adoption off (hermetic runs and measurements).
    pub fn fromEnv(home: ?[]const u8, xdg_config_home: ?[]const u8, override: ?[]const u8) Locator {
        var locator: Locator = .{};
        const home_dir = nonEmpty(home);
        if (home_dir) |value| locator.home.set(value) catch {};
        if (nonEmpty(xdg_config_home)) |value| {
            locator.config_home.set(value) catch {};
        } else if (home_dir) |value| {
            var buffer: [std.fs.max_path_bytes]u8 = undefined;
            if (config.joinDir(value, ".config", &buffer)) |joined| locator.config_home.set(joined) catch {} else |_| {}
        }
        if (override) |value| {
            if (value.len == 0) return locator;
            locator.explicit.set(value) catch return locator;
            locator.mode = .explicit;
            return locator;
        }
        if (locator.config_home.len != 0 or locator.home.len != 0) locator.mode = .search;
        return locator;
    }
};

fn nonEmpty(value: ?[]const u8) ?[]const u8 {
    const text = value orelse return null;
    return if (text.len == 0) null else text;
}

// ------------------------------------------------------------------ parsing

/// Folds Ghostty config text, in order, into an `Inherited`. Several texts
/// (theme, main file, includes) share one accumulator, as Desktop joins them
/// into one blob.
pub const Accumulator = struct {
    inherited: Inherited = .{},
    /// Desktop's `fontReset`: an empty `font-family` clears the primary, and
    /// the next non-empty one becomes primary again.
    font_reset: bool = false,

    pub fn feed(acc: *Accumulator, text: []const u8) void {
        var lines = std.mem.splitScalar(u8, text, '\n');
        while (lines.next()) |raw| {
            const line = std.mem.trim(u8, raw, " \t\r");
            if (line.len == 0 or line[0] == '#') continue;
            const equals = std.mem.indexOfScalar(u8, line, '=') orelse continue;
            const key = std.mem.trim(u8, line[0..equals], " \t");
            const value = unquote(std.mem.trim(u8, line[equals + 1 ..], " \t"));
            acc.apply(key, value);
        }
    }

    fn apply(acc: *Accumulator, key: []const u8, value: []const u8) void {
        const out = &acc.inherited;
        if (std.mem.eql(u8, key, "font-family")) return acc.fontFamily(value);
        if (std.mem.eql(u8, key, "font-size")) {
            const size = std.fmt.parseFloat(f32, value) catch return;
            if (std.math.isFinite(size) and size >= 4 and size <= 72) out.font_size = size;
            return;
        }
        if (std.mem.eql(u8, key, "palette")) return acc.paletteEntry(value);
        const field: *?Rgb = if (std.mem.eql(u8, key, "background"))
            &out.background
        else if (std.mem.eql(u8, key, "foreground"))
            &out.foreground
        else if (std.mem.eql(u8, key, "cursor-color"))
            &out.cursor_color
        else if (std.mem.eql(u8, key, "selection-background"))
            &out.selection_background
        else
            return;
        if (Rgb.parse(std.mem.trim(u8, value, " \t"))) |color| field.* = color;
    }

    /// The first family is primary; later ones are Ghostty's fallbacks.
    fn fontFamily(acc: *Accumulator, value: []const u8) void {
        const out = &acc.inherited;
        if (value.len == 0) {
            acc.font_reset = true;
            out.requested_font = .{};
            return;
        }
        if (out.requested_font.len != 0 and !acc.font_reset) return;
        out.requested_font.set(value) catch return;
        acc.font_reset = false;
    }

    fn paletteEntry(acc: *Accumulator, value: []const u8) void {
        const separator = std.mem.indexOfScalar(u8, value, '=') orelse return;
        const index_text = std.mem.trimEnd(u8, value[0..separator], " \t");
        if (index_text.len == 0) return;
        for (index_text) |c| if (!std.ascii.isDigit(c)) return;
        const index = std.fmt.parseInt(usize, index_text, 10) catch return;
        if (index >= config.palette_len) return;
        const color = Rgb.parse(std.mem.trim(u8, value[separator + 1 ..], " \t")) orelse return;
        acc.inherited.palette[index] = color;
    }

    /// The accumulated layer, stamped with where it came from and with
    /// Ghostty's family mapped onto a face Cockpit ships.
    pub fn finish(acc: *const Accumulator, source: []const u8) Inherited {
        var out = acc.inherited;
        out.source.set(truncateUtf8(source, config.max_inherited_source_bytes)) catch {};
        out.font_family = null;
        if (out.requested_font.len != 0) {
            if (cockpitFamily(out.requested_font.slice())) |family| out.font_family = config.FontFamily.init(family);
        }
        return out;
    }
};

/// Parse one blob of Ghostty config text, as Desktop's `parseGhostty` does.
pub fn parse(text: []const u8, source: []const u8) Inherited {
    var acc: Accumulator = .{};
    acc.feed(text);
    return acc.finish(source);
}

/// Ghostty's family as a Cockpit `font-family` value. Cockpit can draw only
/// the faces it ships, so a Ghostty family is adopted when it names one of
/// them under any of the spellings the family goes by: "JetBrainsMono Nerd
/// Font", "JetBrains Mono" and "JetBrains Mono NL Nerd Font Mono" are the one
/// bundled design. Any other family is reported, never guessed at.
pub fn cockpitFamily(name: []const u8) ?[]const u8 {
    if (config.fontChoice(name)) |choice| return switch (choice) {
        .bundled => "",
        .geist => "Geist Mono",
    };
    var folded: [config.max_font_family_bytes]u8 = undefined;
    var len: usize = 0;
    for (name) |c| {
        if (!std.ascii.isAlphanumeric(c)) continue;
        if (len == folded.len) break;
        folded[len] = std.ascii.toLower(c);
        len += 1;
    }
    const key = folded[0..len];
    if (std.mem.startsWith(u8, key, "jetbrainsmono")) return "";
    if (std.mem.startsWith(u8, key, "geistmono")) return "Geist Mono";
    return null;
}

fn unquote(value: []const u8) []const u8 {
    if (value.len >= 2 and value[0] == '"' and value[value.len - 1] == '"') return value[1 .. value.len - 1];
    return value;
}

fn truncateUtf8(text: []const u8, limit: usize) []const u8 {
    if (text.len <= limit) return text;
    var end = limit;
    while (end > 0 and (text[end] & 0xc0) == 0x80) end -= 1;
    return text[0..end];
}

// ------------------------------------------------------------------ directives

/// The `config-file` path a line names, or null. Ghostty's `?` prefix (an
/// optional include) reads the same as a plain one here: a missing file is
/// skipped either way. Both `"?path"` and `?"path"` are accepted.
pub fn includeTarget(line: []const u8) ?[]const u8 {
    const value = directive(line, "config-file") orelse return null;
    const bare = if (value.len > 0 and value[0] == '?') value[1..] else value;
    const path = unquote(std.mem.trim(u8, bare, " \t"));
    return if (path.len == 0) null else path;
}

/// The value of a `key = value` line when its key is exactly `key`.
fn directive(raw: []const u8, key: []const u8) ?[]const u8 {
    const line = std.mem.trim(u8, raw, " \t\r");
    if (line.len == 0 or line[0] == '#') return null;
    const equals = std.mem.indexOfScalar(u8, line, '=') orelse return null;
    if (!std.mem.eql(u8, std.mem.trim(u8, line[0..equals], " \t"), key)) return null;
    return unquote(std.mem.trim(u8, line[equals + 1 ..], " \t"));
}

/// The theme a `theme =` value selects: `light:x,dark:y` picks the dark one
/// (Cockpit's terminal ground is dark), a bare name is itself, and anything
/// else selects none. Desktop's `themeText` rule.
pub fn themeChoice(value: []const u8) ?[]const u8 {
    var parts = std.mem.splitScalar(u8, value, ',');
    while (parts.next()) |part| {
        const entry = std.mem.trim(u8, part, " \t");
        if (std.mem.startsWith(u8, entry, "dark:")) {
            const name = std.mem.trim(u8, entry["dark:".len..], " \t");
            return if (name.len == 0) null else name;
        }
    }
    if (std.mem.indexOfScalar(u8, value, ':') != null) return null;
    const name = std.mem.trim(u8, value, " \t");
    return if (name.len == 0) null else name;
}

/// The last `theme =` line in `text`, if any, into `out`.
fn lastTheme(text: []const u8, out: *config.Text(max_theme_name_bytes)) void {
    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |line| {
        const value = directive(line, "theme") orelse continue;
        out.set(value) catch {};
    }
}

// ------------------------------------------------------------------ loading

const Buffer = [config.max_config_bytes]u8;

/// Scratch for one load: the main file, one other file at a time, and the
/// resolved include paths. Heap-owned by `load` because it is ~130 KiB.
const Scratch = struct {
    main: Buffer = undefined,
    other: Buffer = undefined,
    main_path: Path = .{},
    includes: [max_includes]Path = [_]Path{.{}} ** max_includes,
    include_count: usize = 0,
    theme: config.Text(max_theme_name_bytes) = .{},
};

/// Read the user's Ghostty config and fold it into an `Inherited`. An empty
/// result (`found() == false`) means there was nothing to adopt.
pub fn load(io: std.Io, locator: *const Locator) Inherited {
    if (locator.mode == .disabled) return .{};
    const scratch = std.heap.page_allocator.create(Scratch) catch return .{};
    defer std.heap.page_allocator.destroy(scratch);
    scratch.* = .{};
    return loadWith(io, locator, scratch);
}

fn loadWith(io: std.Io, locator: *const Locator, scratch: *Scratch) Inherited {
    const main = findMain(io, locator, scratch) orelse return .{};
    const main_path = scratch.main_path.slice();

    // Includes are listed by the main file only, one level deep.
    var lines = std.mem.splitScalar(u8, main, '\n');
    while (lines.next()) |line| {
        if (scratch.include_count == max_includes) break;
        const target = includeTarget(line) orelse continue;
        var joined: [std.fs.max_path_bytes]u8 = undefined;
        const resolved = resolveInclude(main_path, target, locator.home.slice(), &joined) orelse continue;
        scratch.includes[scratch.include_count].set(resolved) catch continue;
        scratch.include_count += 1;
    }

    // The last `theme =` across the main file and its includes names the
    // theme, whose colours go in FIRST so the config's own lines override it.
    lastTheme(main, &scratch.theme);
    for (scratch.includes[0..scratch.include_count]) |*include| {
        const text = readInto(io, include.slice(), &scratch.other) orelse continue;
        lastTheme(text, &scratch.theme);
    }

    var acc: Accumulator = .{};
    if (themeChoice(scratch.theme.slice())) |name| {
        if (readTheme(io, locator, name, &scratch.other)) |text| acc.feed(text);
    }
    acc.feed(main);
    for (scratch.includes[0..scratch.include_count]) |*include| {
        const text = readInto(io, include.slice(), &scratch.other) orelse continue;
        acc.feed(text);
    }

    var display: [std.fs.max_path_bytes]u8 = undefined;
    return acc.finish(abbreviateHome(main_path, locator.home.slice(), &display));
}

/// Ghostty's own search order, as Desktop walks it: the XDG file (plain, then
/// `.ghostty`), then the macOS Application Support copy. The first that opens
/// wins; an explicit override is the only candidate.
fn findMain(io: std.Io, locator: *const Locator, scratch: *Scratch) ?[]const u8 {
    if (locator.mode == .explicit) return tryMain(io, locator.explicit.slice(), scratch);
    const config_home = locator.config_home.slice();
    const home = locator.home.slice();
    const candidates = [_]struct { base: []const u8, dir: []const u8, file: []const u8 }{
        .{ .base = config_home, .dir = "ghostty", .file = "config" },
        .{ .base = config_home, .dir = "ghostty", .file = "config.ghostty" },
        .{ .base = home, .dir = "Library/Application Support/com.mitchellh.ghostty", .file = "config" },
        .{ .base = home, .dir = "Library/Application Support/com.mitchellh.ghostty", .file = "config.ghostty" },
    };
    for (candidates) |candidate| {
        if (candidate.base.len == 0) continue;
        var dir_buffer: [std.fs.max_path_bytes]u8 = undefined;
        var path_buffer: [std.fs.max_path_bytes]u8 = undefined;
        const dir = config.joinDir(candidate.base, candidate.dir, &dir_buffer) catch continue;
        const path = config.joinDir(dir, candidate.file, &path_buffer) catch continue;
        if (tryMain(io, path, scratch)) |text| return text;
    }
    return null;
}

fn tryMain(io: std.Io, path: []const u8, scratch: *Scratch) ?[]const u8 {
    const text = readInto(io, path, &scratch.main) orelse return null;
    scratch.main_path.set(path) catch return null;
    return text;
}

/// `config-file` resolution: absolute as written, `~/` from home, anything
/// else relative to the including file's directory.
pub fn resolveInclude(main_path: []const u8, target: []const u8, home: []const u8, out: []u8) ?[]const u8 {
    if (std.fs.path.isAbsolute(target)) return copyPath(target, out);
    if (std.mem.startsWith(u8, target, "~/")) {
        if (home.len == 0) return null;
        return config.joinDir(home, target[2..], out) catch null;
    }
    const dir = std.fs.path.dirname(main_path) orelse ".";
    return config.joinDir(dir, target, out) catch null;
}

fn copyPath(path: []const u8, out: []u8) ?[]const u8 {
    if (path.len > out.len) return null;
    @memcpy(out[0..path.len], path);
    return out[0..path.len];
}

/// A theme is an absolute path, or a file name in the user's Ghostty theme
/// directory and then Ghostty.app's bundled set.
fn readTheme(io: std.Io, locator: *const Locator, name: []const u8, buffer: *Buffer) ?[]const u8 {
    if (std.fs.path.isAbsolute(name)) return readInto(io, name, buffer);
    // A theme name is a file name, never a path out of the theme directory.
    if (std.mem.indexOfScalar(u8, name, '/') != null) return null;
    var user_dir: [std.fs.max_path_bytes]u8 = undefined;
    const dirs = [_]?[]const u8{
        if (locator.config_home.len != 0) config.joinDir(locator.config_home.slice(), "ghostty/themes", &user_dir) catch null else null,
        "/Applications/Ghostty.app/Contents/Resources/ghostty/themes",
    };
    for (dirs) |maybe_dir| {
        const dir = maybe_dir orelse continue;
        var path: [std.fs.max_path_bytes]u8 = undefined;
        const joined = config.joinDir(dir, name, &path) catch continue;
        if (readInto(io, joined, buffer)) |text| return text;
    }
    return null;
}

/// Read a whole file, truncated at `max_config_bytes` like Cockpit's own
/// config; null when it cannot be opened or read.
fn readInto(io: std.Io, path: []const u8, buffer: *Buffer) ?[]const u8 {
    var file = std.Io.Dir.cwd().openFile(io, path, .{ .allow_directory = false }) catch return null;
    defer file.close(io);
    const length = file.readPositionalAll(io, buffer, 0) catch return null;
    return buffer[0..length];
}

/// `~/…` for a path under home, so the settings line reads like the path a
/// person would type.
pub fn abbreviateHome(path: []const u8, home: []const u8, out: []u8) []const u8 {
    const trimmed_home = std.mem.trimEnd(u8, home, "/");
    if (trimmed_home.len == 0 or !std.mem.startsWith(u8, path, trimmed_home)) return path;
    const rest = path[trimmed_home.len..];
    if (rest.len == 0 or rest[0] != '/') return path;
    if (rest.len + 1 > out.len) return path;
    out[0] = '~';
    @memcpy(out[1..][0..rest.len], rest);
    return out[0 .. rest.len + 1];
}

// ------------------------------------------------------------------ summary

/// One line for Settings saying what was adopted and from where, or empty
/// when nothing was.
pub fn summary(inherited: *const Inherited, out: []u8) []const u8 {
    if (!inherited.found()) return out[0..0];
    var writer: std.Io.Writer = .fixed(out);
    writeSummary(inherited, &writer) catch {};
    return writer.buffered();
}

fn writeSummary(inherited: *const Inherited, writer: *std.Io.Writer) std.Io.Writer.Error!void {
    try writer.print("From Ghostty ({s}):", .{inherited.source.slice()});
    var items: usize = 0;
    if (inherited.font_family != null) {
        try item(writer, &items, "font {s}", .{inherited.requested_font.slice()});
    }
    if (inherited.font_size) |size| try item(writer, &items, "{d} pt", .{size});
    if (inherited.foreground != null or inherited.background != null) try item(writer, &items, "colours", .{});
    if (inherited.cursor_color != null) try item(writer, &items, "cursor colour", .{});
    if (inherited.selection_background != null) try item(writer, &items, "selection colour", .{});
    var palette: usize = 0;
    for (inherited.palette) |entry| palette += @intFromBool(entry != null);
    if (palette != 0) try item(writer, &items, "{d} palette {s}", .{ palette, if (palette == 1) "colour" else "colours" });
    if (items == 0) try writer.writeAll(" nothing Cockpit uses");
    try writer.writeAll(".");
    if (inherited.font_family == null and inherited.requested_font.len != 0) {
        try writer.print(" Font {s} is not one Cockpit ships, so the bundled face stays.", .{inherited.requested_font.slice()});
    }
    try writer.writeAll(" Anything set here takes precedence.");
}

fn item(writer: *std.Io.Writer, count: *usize, comptime format: []const u8, args: anytype) std.Io.Writer.Error!void {
    try writer.writeAll(if (count.* == 0) " " else ", ");
    try writer.print(format, args);
    count.* += 1;
}
