//! User configuration, in Ghostty's syntax: `key = value` per line, `#`
//! comments, and unknown keys as warnings so newer configs still load.
//! Parsing is allocation-free; strings are copied into the `Config`.

const std = @import("std");
const theme_module = @import("theme.zig");
pub const keybindings_module = @import("keybindings.zig");

pub const Theme = theme_module.Theme;

/// Bounds. These are generous for their purpose and keep `Config` a plain
/// value type that can be copied without an allocator.
pub const max_font_family_bytes: usize = 128;
pub const max_shell_bytes: usize = 512;
pub const max_theme_name_bytes: usize = 64;
/// One byte is reserved for the terminating NUL in `sockaddr_un.sun_path`.
/// Accepting anything larger here would defer a deterministic config error
/// until the connection worker starts.
pub const max_phux_socket_bytes: usize = @sizeOf(@FieldType(std.posix.sockaddr.un, "path")) - 1;
/// The Phux protocol's advertised-session admission bound. A configured name
/// must fit the same envelope as the catalog entry it is expected to match.
pub const max_phux_session_bytes: usize = 4096;
/// A `[[remote]]` registry label or `[USER@]HOST[:PORT]`: a host name, not a
/// document. Well under phux-client-ffi's 1024-byte target bound.
pub const max_phux_remote_bytes: usize = 255;
pub const max_diagnostics: usize = 16;
pub const max_config_bytes: usize = 64 * 1024;

/// How much of the offending key or value a diagnostic quotes back.
pub const max_diagnostic_text_bytes: usize = 64;

pub const palette_len: usize = 16;

pub const CursorStyle = enum {
    block,
    bar,
    underline,

    pub fn parse(text: []const u8) ?CursorStyle {
        if (eq(text, "block")) return .block;
        if (eq(text, "bar") or eq(text, "beam")) return .bar;
        if (eq(text, "underline")) return .underline;
        return null;
    }
};

pub const TabPlacement = enum {
    top,
    side,

    pub fn parse(text: []const u8) ?TabPlacement {
        if (eq(text, "top")) return .top;
        if (eq(text, "side") or eq(text, "sidebar")) return .side;
        return null;
    }
};

pub const Rgb = struct {
    r: u8 = 0,
    g: u8 = 0,
    b: u8 = 0,

    pub fn eql(a: Rgb, b: Rgb) bool {
        return a.r == b.r and a.g == b.g and a.b == b.b;
    }

    /// Accepts `#rrggbb`, `rrggbb`, and the `#rgb` shorthand, which is what
    /// people paste out of a theme file.
    pub fn parse(text: []const u8) ?Rgb {
        const body = if (text.len > 0 and text[0] == '#') text[1..] else text;
        switch (body.len) {
            3 => {
                const r = hexDigit(body[0]) orelse return null;
                const g = hexDigit(body[1]) orelse return null;
                const b = hexDigit(body[2]) orelse return null;
                // `#abc` means `#aabbcc`, not `#0a0b0c`.
                return .{ .r = r * 17, .g = g * 17, .b = b * 17 };
            },
            6 => {
                const r = hexByte(body[0..2]) orelse return null;
                const g = hexByte(body[2..4]) orelse return null;
                const b = hexByte(body[4..6]) orelse return null;
                return .{ .r = r, .g = g, .b = b };
            },
            else => return null,
        }
    }
};

/// `theme.Rgb` and `Rgb` are the same three bytes declared in two modules —
/// `theme.zig` cannot import this one without a cycle. This is the one
/// crossing point between them.
fn fromThemeRgb(color: theme_module.Rgb) Rgb {
    return .{ .r = color.r, .g = color.g, .b = color.b };
}

fn hexDigit(c: u8) ?u8 {
    return switch (c) {
        '0'...'9' => c - '0',
        'a'...'f' => c - 'a' + 10,
        'A'...'F' => c - 'A' + 10,
        else => null,
    };
}

fn hexByte(pair: []const u8) ?u8 {
    const high = hexDigit(pair[0]) orelse return null;
    const low = hexDigit(pair[1]) orelse return null;
    return high * 16 + low;
}

fn eq(a: []const u8, b: []const u8) bool {
    return std.ascii.eqlIgnoreCase(a, b);
}

/// A parse problem, kept rather than thrown. A single bad line must not cost
/// someone every other setting in the file.
pub const Diagnostic = struct {
    /// `unsupported_key`: spelled correctly but not honoured by this build,
    /// said out loud rather than silently ignored.
    pub const Kind = enum {
        unknown_key,
        bad_value,
        missing_separator,
        too_long,
        unsupported_key,

        /// The short phrase for the one-line config band.
        pub fn summary(kind: Kind) []const u8 {
            return switch (kind) {
                .unknown_key => "unknown setting",
                .bad_value => "value not understood",
                .missing_separator => "no '=' on this line",
                .too_long => "value too long",
                .unsupported_key => "understood, but does nothing in this build",
            };
        }
    };

    line: u32 = 0,
    kind: Kind = .bad_value,
    /// The offending key or value, copied: the source bytes do not outlive
    /// parsing.
    detail: DiagnosticText = .{},

    /// The offending key or value, or empty when the kind carries none.
    pub fn text(diagnostic: *const Diagnostic) []const u8 {
        return diagnostic.detail.slice();
    }
};

/// A bounded string field that owns its bytes.
pub fn Text(comptime capacity: usize) type {
    return struct {
        const Self = @This();

        bytes: [capacity]u8 = [_]u8{0} ** capacity,
        len: usize = 0,

        pub fn init(value: []const u8) Self {
            var self: Self = .{};
            self.set(value) catch {};
            return self;
        }

        pub fn set(self: *Self, value: []const u8) error{TooLong}!void {
            if (value.len > capacity) return error.TooLong;
            @memcpy(self.bytes[0..value.len], value);
            self.len = value.len;
        }

        pub fn slice(self: *const Self) []const u8 {
            return self.bytes[0..self.len];
        }
    };
}

pub const FontFamily = Text(max_font_family_bytes);
pub const Shell = Text(max_shell_bytes);
pub const ThemeName = Text(max_theme_name_bytes);
pub const DiagnosticText = Text(max_diagnostic_text_bytes);

pub const PhuxSocket = Text(max_phux_socket_bytes);
pub const PhuxSession = Text(max_phux_session_bytes);
pub const PhuxRemote = Text(max_phux_remote_bytes);

/// Where a resolved Phux startup value came from. Settings uses this as
/// provenance only: a transport location is not an authority or trust claim.
pub const PhuxValueSource = enum {
    default,
    config,
    environment,

    pub fn label(source: PhuxValueSource) []const u8 {
        return switch (source) {
            .default => "default",
            .config => "config",
            .environment => "environment",
        };
    }
};

/// Cut `text` to at most `limit` bytes without splitting a UTF-8 sequence.
fn truncateUtf8(text: []const u8, limit: usize) []const u8 {
    if (text.len <= limit) return text;
    var end = limit;
    while (end > 0 and (text[end] & 0xc0) == 0x80) end -= 1;
    return text[0..end];
}

/// Font size bounds. Below 4pt the grid degenerates; above 72pt a default
/// window holds almost no cells. Both ends are clamps, not errors, so a
/// runaway cmd+= cannot wedge the app.
pub const min_font_size: f32 = 4;
pub const max_font_size: f32 = 72;
/// One point above Ghostty's macOS default (13, per `ghostty +show-config
/// --default`). Measured, not guessed: Cockpit already inks glyphs as heavily
/// as Ghostty's maximum `font-thicken` (docs/RENDER_FIDELITY.md section 7), so
/// what made 13 read thin beside the owner's Ghostty was that Ghostty's
/// `font-size = 14`, not the rasterizer. 14 is the legible size, and a Ghostty
/// user's own `font-size` replaces it anyway (`ghostty.zig`).
pub const default_font_size: f32 = 14;

/// Scrollback bounds, in bytes. Ghostty's default is 50 MB and that is what
/// a daily driver needs; the old 1 MB held only a few hundred rows.
pub const default_scrollback_bytes: u64 = 50 * 1024 * 1024;
pub const max_scrollback_bytes: u64 = 2 * 1024 * 1024 * 1024;

/// `minimum-contrast` bounds (Ghostty's): 1 disables the floor, 21 is black on
/// white. Out-of-range values clamp.
pub const min_minimum_contrast: f32 = 1;
pub const max_minimum_contrast: f32 = 21;

/// Default contrast floor; deliberately not Ghostty's 1 (off). Against our
/// #090b0f ground the default palette's illegible entries (SGR 30 black 1.19,
/// faint blue 2.60) sit below 3 and the first legible one (bright black 3.43)
/// above it, so 3 lifts exactly the unreadable cells. Not 4.5: the floor
/// replaces a colour with pure white/black, which would erase de-emphasis greys.
pub const default_minimum_contrast: f32 = 3;

/// How much of a source path the settings surface quotes back.
pub const max_inherited_source_bytes: usize = 256;
pub const InheritedSource = Text(max_inherited_source_bytes);

/// Font and colour defaults adopted from the user's Ghostty config (see
/// `ghostty.zig`). They sit BELOW every Cockpit setting: an explicit key wins,
/// and for foreground, background and selection a named Cockpit theme wins
/// too. Nothing here is ever written back to Cockpit's own file.
pub const Inherited = struct {
    /// The Ghostty file they came from, `~`-abbreviated; empty when no
    /// Ghostty config was found.
    source: InheritedSource = .{},
    /// The Cockpit `font-family` value Ghostty's family maps onto; null when
    /// Ghostty names none, or one Cockpit has no face for.
    font_family: ?FontFamily = null,
    /// Ghostty's primary family exactly as written, adopted or not.
    requested_font: FontFamily = .{},
    font_size: ?f32 = null,
    background: ?Rgb = null,
    foreground: ?Rgb = null,
    cursor_color: ?Rgb = null,
    selection_background: ?Rgb = null,
    palette: [palette_len]?Rgb = [_]?Rgb{null} ** palette_len,

    pub fn found(inherited: *const Inherited) bool {
        return inherited.source.len != 0;
    }
};

pub const Config = struct {
    font_family: FontFamily = FontFamily.init(""),
    font_size: f32 = default_font_size,
    /// The theme in effect; empty for none. Only ever a name `theme.byName`
    /// recognizes.
    theme: ThemeName = ThemeName.init(""),
    /// `theme = auto`: follow the system light/dark setting (`theme` still
    /// names the member in effect).
    follow_system_theme: bool = false,

    /// Explicit `palette = N=#rrggbb` overrides; null keeps the emulator's
    /// palette. Themes never reach it (see `theme.zig`).
    palette: [palette_len]?Rgb = [_]?Rgb{null} ** palette_len,
    /// Explicit keys only (null = not written); read through `resolved*`.
    background: ?Rgb = null,
    foreground: ?Rgb = null,
    cursor_color: ?Rgb = null,
    selection_background: ?Rgb = null,
    selection_foreground: ?Rgb = null,

    cursor_style: CursorStyle = .block,
    cursor_style_blink: bool = true,

    /// WCAG contrast floor for cell foregrounds (1 = off); always clamped to
    /// the valid range by the parser.
    minimum_contrast: f32 = default_minimum_contrast,

    scrollback_bytes: u64 = default_scrollback_bytes,

    /// Empty means "the user's login shell", resolved at spawn time.
    shell: Shell = Shell.init(""),
    /// Local configuration editor command; empty delegates to VISUAL / EDITOR.
    editor: Shell = Shell.init(""),
    keybindings: keybindings_module.Overrides = .{},

    /// Absolute local-domain socket selected by `phux-socket`. Empty only
    /// before startup resolution supplies the platform default.
    phux_socket: PhuxSocket = PhuxSocket.init(""),
    phux_socket_source: PhuxValueSource = .default,
    /// `phux-session` names an already-existing session. Empty means attach
    /// the coordinator's current/Last session; it never means create one.
    phux_session: PhuxSession = PhuxSession.init(""),
    phux_session_source: PhuxValueSource = .default,
    /// `phux-remote` names a host in the phux CLI's own `[[remote]]`
    /// registry (`phux host add|enroll`). Non-empty dials that host instead
    /// of the local coordinator; empty is the local coordinator.
    phux_remote: PhuxRemote = PhuxRemote.init(""),
    phux_remote_source: PhuxValueSource = .default,

    /// A new terminal or split starts in the focused pane's directory, the
    /// way Ghostty does, unless this is turned off.
    inherit_working_directory: bool = true,

    tab_placement: TabPlacement = .top,
    /// At rest a single healthy terminal shows no chrome at all.
    hide_chrome_when_single: bool = true,

    window_padding: f32 = 8,

    /// The Ghostty layer the defaults above were seeded from. Kept so a
    /// Settings reset returns to it and the colour resolvers fall back to it.
    inherited: Inherited = .{},

    diagnostics: [max_diagnostics]Diagnostic = [_]Diagnostic{.{}} ** max_diagnostics,
    diagnostic_count: usize = 0,
    /// Validation is not capped when the display diagnostic buffer is full.
    has_errors: bool = false,

    /// The defaults a Ghostty user starts from: Cockpit's own with the adopted
    /// font in place. Colours stay in `inherited`, read through `resolved*`,
    /// so a Cockpit theme can still outrank them.
    pub fn seeded(inherited: Inherited) Config {
        var config: Config = .{ .inherited = inherited };
        if (inherited.font_size) |size| config.font_size = size;
        if (inherited.font_family) |family| config.font_family = family;
        return config;
    }

    pub fn fontSize(config: *const Config) f32 {
        return std.math.clamp(config.font_size, min_font_size, max_font_size);
    }

    pub fn editorCommand(config: *const Config) []const u8 {
        return config.editor.slice();
    }

    /// The built-in theme this config names, or null when it names none.
    pub fn resolvedTheme(config: *const Config) ?*const Theme {
        const name = config.theme.slice();
        if (name.len == 0) return null;
        return theme_module.byName(name);
    }

    /// Colour precedence: explicit key, then the named theme, then the colour
    /// adopted from Ghostty, then null (the app's own tokens). Resolved at
    /// read time so file order does not matter.
    pub fn resolvedForeground(config: *const Config) ?Rgb {
        if (config.foreground) |explicit| return explicit;
        if (config.resolvedTheme()) |active| return fromThemeRgb(active.foreground);
        return config.inherited.foreground;
    }

    pub fn resolvedBackground(config: *const Config) ?Rgb {
        if (config.background) |explicit| return explicit;
        if (config.resolvedTheme()) |active| return fromThemeRgb(active.background);
        return config.inherited.background;
    }

    pub fn resolvedSelectionBackground(config: *const Config) ?Rgb {
        if (config.selection_background) |explicit| return explicit;
        if (config.resolvedTheme()) |active| return fromThemeRgb(active.selection_background);
        return config.inherited.selection_background;
    }

    /// Themes set neither the cursor nor the palette (see `theme.zig`), so
    /// these fall straight from the explicit key to the adopted one.
    pub fn resolvedCursorColor(config: *const Config) ?Rgb {
        return config.cursor_color orelse config.inherited.cursor_color;
    }

    pub fn resolvedPalette(config: *const Config, index: usize) ?Rgb {
        return config.palette[index] orelse config.inherited.palette[index];
    }

    /// Adopt a known theme by name; false when nothing changed. Naming a theme
    /// ends `theme = auto`.
    pub fn setTheme(config: *Config, name: []const u8) bool {
        if (name.len != 0 and theme_module.byName(name) == null) return false;
        const following = config.follow_system_theme;
        config.follow_system_theme = false;
        if (eq(config.theme.slice(), name)) return following;
        config.theme.set(name) catch return following;
        return true;
    }

    /// Follow the system into `scheme`. A no-op unless `theme = auto` asked
    /// for it, so an appearance event can be delivered unconditionally.
    pub fn adoptSystemTheme(config: *Config, scheme: theme_module.ColorScheme) bool {
        if (!config.follow_system_theme) return false;
        const name = theme_module.forScheme(scheme);
        if (eq(config.theme.slice(), name)) return false;
        config.theme.set(name) catch return false;
        // `setTheme` is deliberately not reused: it ends the subscription, and
        // this IS the subscription.
        return true;
    }

    /// Store an already-validated startup socket together with its provenance.
    /// False leaves the previous safe value intact.
    pub fn setPhuxSocket(config: *Config, value: []const u8, source: PhuxValueSource) bool {
        if (!validPhuxSocket(value)) return false;
        config.phux_socket.set(value) catch return false;
        config.phux_socket_source = source;
        return true;
    }

    /// Store an already-validated existing-session selection. Empty is the
    /// explicit representation of current/Last, not a session named "default".
    pub fn setPhuxSession(config: *Config, value: []const u8, source: PhuxValueSource) bool {
        if (!validPhuxSession(value)) return false;
        config.phux_session.set(value) catch return false;
        config.phux_session_source = if (value.len == 0) .default else source;
        return true;
    }

    /// Store an already-validated remote host label. Empty is the explicit
    /// representation of the local coordinator.
    pub fn setPhuxRemote(config: *Config, value: []const u8, source: PhuxValueSource) bool {
        if (!validPhuxRemote(value)) return false;
        config.phux_remote.set(value) catch return false;
        config.phux_remote_source = if (value.len == 0) .default else source;
        return true;
    }

    fn note(config: *Config, line: u32, kind: Diagnostic.Kind, text: []const u8) void {
        if (kind == .bad_value or kind == .missing_separator or kind == .too_long) config.has_errors = true;
        if (config.diagnostic_count >= max_diagnostics) return;
        var entry: Diagnostic = .{ .line = line, .kind = kind };
        // Truncating rather than refusing: the text is context, and a
        // diagnostic that dropped its context because the offending value was
        // long is a diagnostic that failed at the one moment it was needed.
        entry.detail.set(truncateUtf8(text, max_diagnostic_text_bytes)) catch {};
        config.diagnostics[config.diagnostic_count] = entry;
        config.diagnostic_count += 1;
    }

    pub fn diagnosticSlice(config: *const Config) []const Diagnostic {
        return config.diagnostics[0..config.diagnostic_count];
    }
};

pub fn validPhuxSocket(value: []const u8) bool {
    return value.len != 0 and
        value.len <= max_phux_socket_bytes and
        std.fs.path.isAbsolute(value) and
        std.mem.indexOfScalar(u8, value, 0) == null and
        std.unicode.utf8ValidateSlice(value);
}

/// Shape only; the registry decides what a label means. Empty is valid (the
/// local coordinator). Whitespace, control bytes and URIs are refused here,
/// where a config line number can still be reported: a URI belongs in
/// `phux host add NAME URI`, and Cockpit connects by NAME.
pub fn validPhuxRemote(value: []const u8) bool {
    if (value.len > max_phux_remote_bytes) return false;
    if (!std.unicode.utf8ValidateSlice(value)) return false;
    if (std.mem.indexOf(u8, value, "://") != null) return false;
    for (value) |byte| if (byte <= ' ' or byte == 0x7f) return false;
    return true;
}

pub fn validPhuxSession(value: []const u8) bool {
    return value.len <= max_phux_session_bytes and
        std.mem.indexOfScalar(u8, value, 0) == null and
        std.unicode.utf8ValidateSlice(value);
}

/// Parse a config file's bytes. Never fails: a malformed line becomes a
/// diagnostic and the remaining lines still apply. This is deliberate —
/// a typo in one setting must not drop someone into a default terminal.
pub fn parse(source: []const u8) Config {
    return parseOver(.{}, source);
}

/// `parse` over defaults seeded from an adopted Ghostty layer. Every line of
/// `source` outranks the layer; diagnostics describe `source` alone.
pub fn parseOver(inherited: Inherited, source: []const u8) Config {
    var config = Config.seeded(inherited);
    var line_number: u32 = 0;
    var lines = std.mem.splitScalar(u8, source, '\n');
    while (lines.next()) |raw_line| {
        line_number += 1;
        const line = trim(stripComment(raw_line));
        if (line.len == 0) continue;

        const separator = std.mem.indexOfScalar(u8, line, '=') orelse {
            config.note(line_number, .missing_separator, line);
            continue;
        };
        const key = trim(line[0..separator]);
        const value = trim(line[separator + 1 ..]);
        if (key.len == 0) {
            config.note(line_number, .missing_separator, line);
            continue;
        }
        applyPair(&config, line_number, key, value);
    }
    return config;
}

fn applyPair(config: *Config, line: u32, key: []const u8, value: []const u8) void {
    if (std.mem.startsWith(u8, key, "keybind.")) return applyKeybinding(config, line, key[8..], value);
    if (applyTextPair(config, line, key, value)) return;
    if (applyNumericPair(config, line, key, value)) return;
    if (applyColorPair(config, line, key, value)) return;
    if (applyBehaviorPair(config, line, key, value)) return;
    if (applyPhuxPair(config, line, key, value)) return;
    if (eq(key, "theme")) return applyTheme(config, line, value);
    // `palette = N=#rrggbb` is the one compound key, and it is the shape
    // Ghostty themes are written in, so it is worth the special case.
    if (eq(key, "palette")) {
        applyPalette(config, line, value);
        return;
    }

    config.note(line, .unknown_key, key);
}

fn applyKeybinding(config: *Config, line: u32, id: []const u8, value: []const u8) void {
    config.keybindings.set(id, value) catch |err| {
        config.note(line, if (err == error.TooLong) .too_long else .bad_value, id);
    };
}

fn applyTextPair(config: *Config, line: u32, key: []const u8, value: []const u8) bool {
    if (eq(key, "editor")) {
        config.editor.set(value) catch config.note(line, .too_long, value);
        return true;
    }
    if (eq(key, "shell") or eq(key, "command")) {
        config.shell.set(value) catch config.note(line, .too_long, value);
        return true;
    }
    if (eq(key, "font-family")) {
        // The native host registers the supported family choices by FontId.
        // Unknown names remain readable, with an explicit unsupported warning.
        config.font_family.set(value) catch {
            config.note(line, .too_long, value);
            return true;
        };
        if (fontChoice(value) == null) config.note(line, .unsupported_key, key);
        return true;
    }
    return false;
}

fn applyNumericPair(config: *Config, line: u32, key: []const u8, value: []const u8) bool {
    if (eq(key, "font-size")) {
        setNumber(config, line, &config.font_size, value, min_font_size, max_font_size, true);
        return true;
    }
    if (eq(key, "minimum-contrast")) {
        setNumber(config, line, &config.minimum_contrast, value, min_minimum_contrast, max_minimum_contrast, true);
        return true;
    }
    if (eq(key, "window-padding")) {
        setNumber(config, line, &config.window_padding, value, 0, 64, false);
        return true;
    }
    if (eq(key, "scrollback-limit")) {
        const parsed = std.fmt.parseInt(u64, value, 10) catch {
            config.note(line, .bad_value, value);
            return true;
        };
        config.scrollback_bytes = @min(parsed, max_scrollback_bytes);
        return true;
    }
    return false;
}

fn setNumber(config: *Config, line: u32, field: *f32, value: []const u8, min: f32, max: f32, clamp: bool) void {
    const parsed = std.fmt.parseFloat(f32, value) catch {
        config.note(line, .bad_value, value);
        return;
    };
    if (std.math.isNan(parsed)) return config.note(line, .bad_value, value);
    if (!clamp and (parsed < min or parsed > max)) return config.note(line, .bad_value, value);
    if (field == &config.font_size and !std.math.isFinite(parsed)) return config.note(line, .bad_value, value);
    field.* = std.math.clamp(parsed, min, max);
}

fn applyTheme(config: *Config, line: u32, value: []const u8) void {
    if (value.len == 0) {
        config.theme = .{};
        config.follow_system_theme = false;
        return;
    }
    // `auto` follows system appearance; the name stored here only lasts
    // until the first appearance event.
    if (theme_module.isAutoName(value)) {
        config.follow_system_theme = true;
        config.theme.set(theme_module.auto_dark) catch config.note(line, .too_long, value);
        return;
    }
    // An unknown name is a `bad_value` and is not stored.
    if (theme_module.byName(value) == null) {
        config.note(line, .bad_value, value);
        return;
    }
    config.theme.set(value) catch config.note(line, .too_long, value);
    config.follow_system_theme = false;
    return;
}

fn applyColorPair(config: *Config, line: u32, key: []const u8, value: []const u8) bool {
    inline for (.{ .{ "background", "background" }, .{ "foreground", "foreground" }, .{ "cursor-color", "cursor_color" }, .{ "selection-background", "selection_background" } }) |pair| {
        if (eq(key, pair[0])) {
            setColor(config, line, &@field(config, pair[1]), value);
            return true;
        }
    }
    if (eq(key, "selection-foreground")) {
        // Inert until the SDK grid carries a selection foreground. A bad value
        // takes precedence over the unsupported note (one diagnostic per line).
        setColor(config, line, &config.selection_foreground, value);
        if (config.selection_foreground != null) config.note(line, .unsupported_key, key);
        return true;
    }
    return false;
}

fn applyBehaviorPair(config: *Config, line: u32, key: []const u8, value: []const u8) bool {
    if (eq(key, "cursor-style")) {
        config.cursor_style = CursorStyle.parse(value) orelse {
            config.note(line, .bad_value, value);
            return true;
        };
        return true;
    }
    inline for (.{ .{ "cursor-style-blink", "cursor_style_blink" }, .{ "inherit-working-directory", "inherit_working_directory" }, .{ "hide-chrome-when-single", "hide_chrome_when_single" } }) |pair| {
        if (eq(key, pair[0])) {
            setBool(config, line, &@field(config, pair[1]), value);
            return true;
        }
    }
    if (eq(key, "tab-placement")) {
        config.tab_placement = TabPlacement.parse(value) orelse {
            config.note(line, .bad_value, value);
            return true;
        };
        return true;
    }
    return false;
}

/// Registered native monospace families; arbitrary names remain diagnostic.
pub const FontChoice = enum { bundled, geist };
pub fn fontChoice(value: []const u8) ?FontChoice {
    if (value.len == 0) return .bundled;
    if (eq(value, "JetBrains Mono NL Nerd Font Mono")) return .bundled;
    if (eq(value, "Geist Mono")) return .geist;
    return null;
}

fn applyPalette(config: *Config, line: u32, value: []const u8) void {
    const separator = std.mem.indexOfScalar(u8, value, '=') orelse {
        config.note(line, .bad_value, value);
        return;
    };
    const index_text = trim(value[0..separator]);
    const color_text = trim(value[separator + 1 ..]);
    const index = std.fmt.parseInt(usize, index_text, 10) catch {
        config.note(line, .bad_value, value);
        return;
    };
    // Only the ANSI-16 range is overridable here; 16-255 is the standard
    // cube and greyscale ramp, which the engine derives.
    if (index >= palette_len) {
        config.note(line, .bad_value, value);
        return;
    }
    const color = Rgb.parse(color_text) orelse {
        config.note(line, .bad_value, value);
        return;
    };
    config.palette[index] = color;
}

fn setColor(config: *Config, line: u32, field: *?Rgb, value: []const u8) void {
    field.* = Rgb.parse(value) orelse {
        config.note(line, .bad_value, value);
        return;
    };
}

fn setBool(config: *Config, line: u32, field: *bool, value: []const u8) void {
    if (eq(value, "true") or eq(value, "yes") or eq(value, "1") or eq(value, "on")) {
        field.* = true;
        return;
    }
    if (eq(value, "false") or eq(value, "no") or eq(value, "0") or eq(value, "off")) {
        field.* = false;
        return;
    }
    config.note(line, .bad_value, value);
}

fn applyPhuxPair(config: *Config, line: u32, key: []const u8, value: []const u8) bool {
    if (eq(key, "phux-socket")) {
        setPhuxSocket(config, line, value);
        return true;
    }
    if (eq(key, "phux-session")) {
        setPhuxSession(config, line, value);
        return true;
    }
    if (eq(key, "phux-remote")) {
        setPhuxRemote(config, line, value);
        return true;
    }
    return false;
}

fn setPhuxRemote(config: *Config, line: u32, value: []const u8) void {
    const detail = phuxDiagnosticText("phux-remote", value);
    if (value.len > max_phux_remote_bytes) {
        config.note(line, .too_long, detail);
        return;
    }
    if (!config.setPhuxRemote(value, .config)) config.note(line, .bad_value, detail);
}

fn phuxDiagnosticText(key: []const u8, value: []const u8) []const u8 {
    if (std.mem.indexOfScalar(u8, value, 0) != null) return key;
    if (!std.unicode.utf8ValidateSlice(value)) return key;
    return value;
}

fn setPhuxSocket(config: *Config, line: u32, value: []const u8) void {
    const detail = phuxDiagnosticText("phux-socket", value);
    if (value.len > max_phux_socket_bytes) {
        config.note(line, .too_long, detail);
        return;
    }
    if (!config.setPhuxSocket(value, .config)) config.note(line, .bad_value, detail);
}

fn setPhuxSession(config: *Config, line: u32, value: []const u8) void {
    const detail = phuxDiagnosticText("phux-session", value);
    if (value.len > max_phux_session_bytes) {
        config.note(line, .too_long, detail);
        return;
    }
    if (!config.setPhuxSession(value, .config)) config.note(line, .bad_value, detail);
}

/// `#` starts a comment only at the start of a line (as in Ghostty), since
/// colour values begin with `#`.
fn stripComment(line: []const u8) []const u8 {
    const body = std.mem.trimStart(u8, line, " \t");
    if (body.len > 0 and body[0] == '#') return line[0..0];
    return line;
}

fn trim(text: []const u8) []const u8 {
    return std.mem.trim(u8, text, " \t\r");
}

/// The file name inside the resolved config directory. The caller resolves
/// the directory itself (the SDK's `app_dirs` knows the platform rules), so
/// this module keeps no ambient dependency and stays unit-testable.
pub const file_name = "config";

/// Join a directory with a child component, normalizing a trailing slash.
/// Path building lives here rather than at the call site so the config file's
/// several candidate locations are assembled one way.
pub fn joinDir(base: []const u8, child: []const u8, output: []u8) error{NoSpaceLeft}![]const u8 {
    const separator: []const u8 = if (base.len > 0 and base[base.len - 1] == '/') "" else "/";
    const total = base.len + separator.len + child.len;
    if (total > output.len) return error.NoSpaceLeft;
    @memcpy(output[0..base.len], base);
    @memcpy(output[base.len..][0..separator.len], separator);
    @memcpy(output[base.len + separator.len ..][0..child.len], child);
    return output[0..total];
}

/// Join a resolved config directory with the config file name.
pub fn joinPath(config_dir: []const u8, output: []u8) error{NoSpaceLeft}![]const u8 {
    const separator: []const u8 = if (config_dir.len > 0 and config_dir[config_dir.len - 1] == '/') "" else "/";
    const total = config_dir.len + separator.len + file_name.len;
    if (total > output.len) return error.NoSpaceLeft;
    @memcpy(output[0..config_dir.len], config_dir);
    @memcpy(output[config_dir.len..][0..separator.len], separator);
    @memcpy(output[config_dir.len + separator.len ..][0..file_name.len], file_name);
    return output[0..total];
}

/// Parse bytes the caller read, or defaults when there were none.
pub fn loadOrDefault(bytes: ?[]const u8) Config {
    return loadOver(.{}, bytes);
}

/// `loadOrDefault` over an adopted Ghostty layer.
pub fn loadOver(inherited: Inherited, bytes: ?[]const u8) Config {
    return parseOver(inherited, bytes orelse return Config.seeded(inherited));
}

test "last explicit theme assignment ends system following" {
    const parsed = parse("theme = auto\ntheme = nord\n");
    try std.testing.expectEqualStrings("nord", parsed.theme.slice());
    try std.testing.expect(!parsed.follow_system_theme);
}

test "supported font choices and editor preference are real typed values" {
    const parsed = parse("font-family = Geist Mono\neditor = nvim --wait\n");
    try std.testing.expectEqual(@as(usize, 0), parsed.diagnostic_count);
    try std.testing.expectEqual(FontChoice.geist, fontChoice(parsed.font_family.slice()).?);
    try std.testing.expectEqualStrings("nvim --wait", parsed.editorCommand());
    try std.testing.expectEqual(FontChoice.bundled, fontChoice("").?);
    const unsupported = parse("font-family = Unregistered Face\n");
    try std.testing.expectEqual(Diagnostic.Kind.unsupported_key, unsupported.diagnostics[0].kind);
}

test "binding config parses canonical values and retains syntax diagnostics" {
    const parsed = parse("keybind.view.settings = Cmd+Alt+s\nkeybind.view.clear = none\n");
    try std.testing.expectEqual(@as(usize, 0), parsed.diagnostic_count);
    try std.testing.expectEqual(@as(usize, 2), parsed.keybindings.count);
    const reset = parse("keybind.view.settings = Cmd+Alt+s\nkeybind.view.settings = default\n");
    try std.testing.expectEqual(@as(usize, 0), reset.keybindings.count);
    const invalid = parse("keybind.view.settings = Ctrl+s\n");
    try std.testing.expectEqual(Diagnostic.Kind.bad_value, invalid.diagnostics[0].kind);
}

test "reset removes only live assignments and preserves comment bytes" {
    const original = "# keybind.view.settings = Cmd+,\r\nkeybind.view.settings=Cmd+Alt+s\r\nfuture = keep\r\nkeybind.view.settings = none";
    var buffer: [512]u8 = undefined;
    const result = try removeKey(original, "keybind.view.settings", &buffer);
    try std.testing.expectEqualStrings("# keybind.view.settings = Cmd+,\r\nfuture = keep\r\n", result);
}

test "validation retains errors beyond the visible diagnostic cap and binding capacity" {
    const saturated = parse("future-key = keep\n" ** max_diagnostics ++ "font-size = broken\n");
    try std.testing.expectEqual(max_diagnostics, saturated.diagnostic_count);
    try std.testing.expect(saturated.has_errors);
    var buffer: [8192]u8 = undefined;
    var length: usize = 0;
    for (0..keybindings_module.max_overrides + 1) |id| {
        const line = try std.fmt.bufPrint(buffer[length..], "keybind.command{d} = none\n", .{id});
        length += line.len;
    }
    const over_capacity = parse(buffer[0..length]);
    try std.testing.expect(over_capacity.has_errors);
}

// ------------------------------------------------------------------ writing

/// Rewrite a hand-written config's bytes so `key` reads `value`, editing text
/// rather than re-serializing so comments, order and unknown keys survive.
/// The first matching line is replaced in place; later duplicates are dropped
/// (the parser is last-wins); a missing key is appended. Commented-out lines
/// are left alone.
pub fn setKey(
    source: []const u8,
    key: []const u8,
    value: []const u8,
    output: []u8,
) error{NoSpaceLeft}![]const u8 {
    var writer = Writer{ .buffer = output };
    var replaced = false;

    var cursor: usize = 0;
    while (cursor < source.len) {
        // The line INCLUDING its terminator, so `\r\n` and a final line with
        // no newline at all both survive a rewrite unchanged.
        const newline = std.mem.indexOfScalarPos(u8, source, cursor, '\n');
        const end = if (newline) |index| index + 1 else source.len;
        const whole = source[cursor..end];
        cursor = end;

        if (!lineNamesKey(whole, key)) {
            try writer.write(whole);
            continue;
        }
        if (replaced) continue; // A later duplicate; see the comment above.
        replaced = true;
        try writer.write(key);
        try writer.write(" = ");
        try writer.write(value);
        // Keep whatever terminator the original line had, so a CRLF file
        // stays a CRLF file and a final line without a newline stays one.
        if (std.mem.endsWith(u8, whole, "\r\n")) {
            try writer.write("\r\n");
        } else if (std.mem.endsWith(u8, whole, "\n")) {
            try writer.write("\n");
        }
    }

    if (!replaced) {
        if (writer.len != 0 and output[writer.len - 1] != '\n') try writer.write("\n");
        try writer.write(key);
        try writer.write(" = ");
        try writer.write(value);
        try writer.write("\n");
    }
    return output[0..writer.len];
}

/// Whether a raw line is a live `key = ...` assignment for `key`. Comments,
/// blank lines, and separator-less lines are all "no" — they are somebody
/// else's bytes and are copied through untouched.
fn lineNamesKey(raw_line: []const u8, key: []const u8) bool {
    const line = trim(stripComment(std.mem.trimEnd(u8, raw_line, "\n")));
    if (line.len == 0) return false;
    const separator = std.mem.indexOfScalar(u8, line, '=') orelse return false;
    return eq(trim(line[0..separator]), key);
}

/// Remove only live assignments; comments and all unrelated bytes survive.
pub fn removeKey(source: []const u8, key: []const u8, output: []u8) error{NoSpaceLeft}![]const u8 {
    var writer = Writer{ .buffer = output };
    var cursor: usize = 0;
    while (cursor < source.len) {
        const newline = std.mem.indexOfScalarPos(u8, source, cursor, '\n');
        const end = if (newline) |index| index + 1 else source.len;
        const whole = source[cursor..end];
        cursor = end;
        if (!lineNamesKey(whole, key)) try writer.write(whole);
    }
    return output[0..writer.len];
}

/// A bounds-checked append into a caller-owned buffer. The whole rewriter is
/// allocation-free for the same reason the parser is: it runs from `update`,
/// which has no allocator.
const Writer = struct {
    buffer: []u8,
    len: usize = 0,

    fn write(self: *Writer, bytes: []const u8) error{NoSpaceLeft}!void {
        if (self.len + bytes.len > self.buffer.len) return error.NoSpaceLeft;
        @memcpy(self.buffer[self.len..][0..bytes.len], bytes);
        self.len += bytes.len;
    }
};
