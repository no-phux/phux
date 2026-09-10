//! User configuration.
//!
//! A terminal someone actually lives in has to be theirs: their font, their
//! size, their colors, their shell. Until now Cockpit had no knob at all —
//! every one of these was a literal in the source.
//!
//! The syntax is deliberately Ghostty's, because that is what the people
//! switching already have in their fingers: one `key = value` per line,
//! `#` starts a comment, blank lines are ignored, and an unknown key is a
//! warning rather than a fatal error so a config written for a newer build
//! still loads on an older one.
//!
//! Parsing is allocation-free. Strings are copied into fixed buffers owned
//! by the `Config` itself, so a loaded config outlives the file bytes and
//! can be embedded directly in the model.

const std = @import("std");
const theme_module = @import("theme.zig");

pub const Theme = theme_module.Theme;
pub const builtin_themes = theme_module.builtins;

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

/// How much of the offending key or value a diagnostic carries.
///
/// The longest key this parser accepts fits comfortably inside 64 bytes, as do
/// the values worth quoting back (a colour, a size, a cursor style, a short
/// command). It is deliberately not the size of the largest configurable
/// string: this storage is paid `max_diagnostics` times inside a `Config` that
/// is copied by value into the model.
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

/// The reverse crossing, for callers measuring a resolved colour's contrast.
pub fn toThemeRgb(color: Rgb) theme_module.Rgb {
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
    /// `unsupported_key` is NOT `unknown_key`. The key is spelled correctly and
    /// parses; this build simply cannot honour it, and says so rather than
    /// letting someone believe a setting took effect. A knob that accepts a
    /// value and does nothing is worse than one that is missing, because the
    /// missing one sends you to the docs and the silent one sends you hunting.
    pub const Kind = enum {
        unknown_key,
        bad_value,
        missing_separator,
        too_long,
        unsupported_key,

        /// The phrase a one-line surface uses. Short because it has to share a
        /// band with a line number and a dismiss control; the startup log
        /// spells the same five cases out as whole sentences, where there is
        /// room for them.
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
    /// The offending key or value, OWNED.
    ///
    /// It used to be a slice borrowed from the source bytes, and nothing that
    /// reads a diagnostic is alive while those bytes are: the composition root
    /// reads the file into a buffer local to the read, parses, and returns the
    /// `Config` BY VALUE — so every borrowed slice pointed into a dead stack
    /// frame before the first log line was even formatted, and the model keeps
    /// the same `Config` for the life of the process. A bounded copy is what
    /// makes a diagnostic mean something later, which is the whole point of
    /// collecting one.
    detail: DiagnosticText = .{},

    /// The offending key or value, or empty when the kind carries none.
    pub fn text(diagnostic: *const Diagnostic) []const u8 {
        return diagnostic.detail.slice();
    }
};

/// A bounded string field that owns its bytes.
fn Text(comptime capacity: usize) type {
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

/// Cut `text` to at most `limit` bytes WITHOUT splitting a UTF-8 sequence.
///
/// A diagnostic's text is quoted straight back into a log line and an
/// accessibility label, and a label that ends mid-codepoint is a label with an
/// invalid byte in it. Walking back off continuation bytes costs at most three
/// steps and removes the whole class.
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
pub const default_font_size: f32 = 13;

/// Scrollback bounds, in bytes. Ghostty's default is 50 MB and that is what
/// a daily driver needs; the old 1 MB held only a few hundred rows.
pub const default_scrollback_bytes: u64 = 50 * 1024 * 1024;
pub const max_scrollback_bytes: u64 = 2 * 1024 * 1024 * 1024;

/// `minimum-contrast` bounds. Ghostty's own range, and its own meaning: 1 is
/// "no floor at all" (the disable value) and 21 is the most any sRGB display
/// can produce, black on white. Out-of-range is CLAMPED rather than refused,
/// the way `font-size` is — `minimum-contrast = 100` is an unambiguous "as
/// much as you can give me".
pub const min_minimum_contrast: f32 = 1;
pub const max_minimum_contrast: f32 = 21;

/// The default contrast floor, and the ONE place this build knowingly diverges
/// from Ghostty (whose default is 1, i.e. off).
///
/// WHY DIVERGE. Ghostty ships 1 because a general-purpose terminal's first
/// duty is to paint the colour the application asked for. Cockpit inherits
/// that duty and still has a defect Ghostty does not: its ground is #090b0f,
/// which is darker than any ground the ANSI-16 defaults were calibrated
/// against, and the owner's standing report is "the text is see-through or
/// black or something". A floor that ships disabled does not answer that
/// report — it answers it only for the person who already knows the key name.
///
/// WHY 3 AND NOT SOMETHING ELSE. Measured, not chosen. Every ratio below is
/// libghostty's own default palette entry against the app's own default
/// ground, through `theme.contrastRatio`; reproduce with
///
///   python3 - <<'PY'
///   def lin(c):
///       c /= 255.0
///       return c/12.92 if c <= 0.03928 else ((c+0.055)/1.055)**2.4
///   def L(c): return 0.2126*lin(c[0]) + 0.7152*lin(c[1]) + 0.0722*lin(c[2])
///   def ratio(a, b):
///       la, lb = L(a), L(b)
///       return (max(la,lb)+0.05)/(min(la,lb)+0.05)
///   bg = (0x09,0x0b,0x0f)
///   for name, c in [("SGR 30 black",(0x1d,0x1f,0x21)),
///                   ("SGR 2 faint blue",(0x45,0x56,0x66)),
///                   ("SGR 90 bright black",(0x66,0x66,0x66)),
///                   ("SGR 2 faint default fg",(0x7e,0x81,0x85)),
///                   ("SGR 31 red",(0xcc,0x66,0x66))]:
///       print(name, round(ratio(c, bg), 2))
///   PY
///
///   SGR 30 black            #1d1f21   1.19    illegible
///   SGR 2 faint blue        #455666   2.60    illegible
///   SGR 90 bright black     #666666   3.43    dim, but readable
///   SGR 2 faint default fg  #7e8185   5.03    readable (clears WCAG AA)
///   SGR 31 red              #cc6666   5.31    readable
///
/// The two illegible cases sit at or below 2.60 and the first legible one at
/// 3.43, so any floor in (2.60, 3.43] lifts exactly what is unreadable and
/// touches nothing else. 3 is the round number inside that window, and it is
/// also WCAG SC 1.4.11's non-text-contrast minimum — the line below which a
/// shape stops being distinguishable from its ground at all.
///
/// It is deliberately NOT `theme.wcag_aa_body_text` (4.5). This floor does not
/// dim a colour towards readability, it REPLACES it with pure white or pure
/// black (see `Palette.contrasted`), so every raised cell loses its hue. At
/// 4.5 the grey a prompt uses for de-emphasis becomes the same pure white as
/// the text it was de-emphasising, which destroys information rather than
/// revealing it. At 3 the grey stays grey.
pub const default_minimum_contrast: f32 = 3;

pub const Config = struct {
    font_family: FontFamily = FontFamily.init(""),
    font_size: f32 = default_font_size,
    /// The name from `theme = <name>`, empty for "no theme named". Only ever
    /// holds a name `theme.byName` recognizes: an unrecognized one is a
    /// `bad_value` diagnostic and is NOT stored, so nothing downstream has to
    /// re-check whether the name means anything.
    theme: ThemeName = ThemeName.init(""),
    /// `theme = auto`: the theme FOLLOWS the system light/dark setting instead
    /// of naming one.
    ///
    /// It is a second field rather than a magic value inside `theme` because
    /// everything downstream reads `theme` expecting a name it can resolve —
    /// storing "auto" there would put a string that means nothing to
    /// `theme.byName` in front of every colour lookup in the app. Here, the
    /// pair reads honestly: `theme` is always the theme in effect right now,
    /// and this says who chose it.
    follow_system_theme: bool = false,

    /// Null means "the palette the terminal engine ships", which is a real
    /// terminal palette. Only an explicit `palette = N=#rrggbb` overrides it.
    ///
    /// Deliberately NOT reached by `theme` — see the module comment on
    /// `theme.zig` for why the ANSI-16 slots stay the emulator's.
    palette: [palette_len]?Rgb = [_]?Rgb{null} ** palette_len,
    /// The EXPLICIT `background` / `foreground` keys. Null means the user did
    /// not write one, which is not the same as "black": it is what lets a
    /// theme fill in underneath, and what lets an explicit key outrank one.
    /// Everything downstream reads `resolvedBackground` / `resolvedForeground`
    /// rather than these, so the precedence lives in ONE place.
    background: ?Rgb = null,
    foreground: ?Rgb = null,
    cursor_color: ?Rgb = null,
    selection_background: ?Rgb = null,
    selection_foreground: ?Rgb = null,

    cursor_style: CursorStyle = .block,
    cursor_style_blink: bool = true,

    /// The WCAG contrast ratio a cell's foreground must clear against the
    /// background it is painted on. 1 disables the floor entirely, which is
    /// Ghostty's default and this build's OFF switch rather than its default;
    /// see `default_minimum_contrast` for why the default is 3 here.
    ///
    /// Always in `[min_minimum_contrast, max_minimum_contrast]`: the parser
    /// clamps, so nothing downstream re-checks the range.
    minimum_contrast: f32 = default_minimum_contrast,

    scrollback_bytes: u64 = default_scrollback_bytes,

    /// Empty means "the user's login shell", resolved at spawn time.
    shell: Shell = Shell.init(""),

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

    diagnostics: [max_diagnostics]Diagnostic = [_]Diagnostic{.{}} ** max_diagnostics,
    diagnostic_count: usize = 0,

    pub fn fontSize(config: *const Config) f32 {
        return std.math.clamp(config.font_size, min_font_size, max_font_size);
    }

    /// The built-in theme this config names, or null when it names none.
    pub fn resolvedTheme(config: *const Config) ?*const Theme {
        const name = config.theme.slice();
        if (name.len == 0) return null;
        return theme_module.byName(name);
    }

    /// THE colour precedence, stated once.
    ///
    ///   1. an explicit `foreground` / `background` / `selection-background`
    ///      key, because someone who wrote a hex value meant that hex value;
    ///   2. the named theme's own colour;
    ///   3. null — meaning "the app's own register", which the design tokens
    ///      already hold.
    ///
    /// Resolved HERE rather than at parse time on purpose. Doing it at parse
    /// time would make the file ORDER-SENSITIVE: `foreground = #ff0000` above
    /// `theme = nord` would lose and the same two lines swapped would win,
    /// which is a rule nobody can hold in their head and nothing in the file
    /// makes visible. Keeping both and deciding at the read site means an
    /// explicit key outranks a theme wherever the two happen to sit.
    ///
    /// Every one of the three flows on to the terminal through the design
    /// tokens (`terminalTokensFrom`), which are rebuilt every frame — so a
    /// mutation of `Model.config` repaints on the next frame with nothing to
    /// invalidate.
    pub fn resolvedForeground(config: *const Config) ?Rgb {
        if (config.foreground) |explicit| return explicit;
        const active = config.resolvedTheme() orelse return null;
        return fromThemeRgb(active.foreground);
    }

    pub fn resolvedBackground(config: *const Config) ?Rgb {
        if (config.background) |explicit| return explicit;
        const active = config.resolvedTheme() orelse return null;
        return fromThemeRgb(active.background);
    }

    pub fn resolvedSelectionBackground(config: *const Config) ?Rgb {
        if (config.selection_background) |explicit| return explicit;
        const active = config.resolvedTheme() orelse return null;
        return fromThemeRgb(active.selection_background);
    }

    /// Adopt a theme by name, dropping an unknown one rather than storing it.
    /// False means nothing changed, which is what the settings surface needs
    /// to know before it decides whether to write the file.
    ///
    /// Naming a theme ENDS the subscription: picking one in the settings
    /// surface while `theme = auto` was in effect is a choice, and the next
    /// sunset must not overrule it. Writing the config file is the settings
    /// surface's own step — it writes the NAME, so the file stops saying
    /// `auto` at exactly the moment the app stops following.
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

    /// Font sizing steps by whole points, which is what cmd+= / cmd+- do in
    /// every Mac terminal. Returns the clamped result so callers can tell
    /// when they hit the end of the range.
    pub fn withFontSize(config: Config, size: f32) Config {
        var next = config;
        next.font_size = std.math.clamp(size, min_font_size, max_font_size);
        return next;
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
    var config: Config = .{};
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
    // `palette = N=#rrggbb` is the one compound key, and it is the shape
    // Ghostty themes are written in, so it is worth the special case.
    if (eq(key, "palette")) {
        applyPalette(config, line, value);
        return;
    }

    if (eq(key, "font-family")) {
        // Parsed and STORED, but it cannot take effect: the SDK selects faces
        // from a fixed set registered at boot by FontId, and mono_font_family
        // is a 4-value enum rather than a family name. There is no runtime
        // load-family-by-name to call. Kept in the struct so the value is
        // still readable (and so this becomes a one-line change the day the
        // SDK grows the capability), but the user is told it did nothing.
        config.font_family.set(value) catch {
            config.note(line, .too_long, value);
            return;
        };
        config.note(line, .unsupported_key, key);
        return;
    }
    if (eq(key, "font-size")) {
        const parsed = std.fmt.parseFloat(f32, value) catch {
            config.note(line, .bad_value, value);
            return;
        };
        if (!std.math.isFinite(parsed)) {
            config.note(line, .bad_value, value);
            return;
        }
        // Out-of-range is clamped rather than refused: the intent is clear.
        config.font_size = std.math.clamp(parsed, min_font_size, max_font_size);
        return;
    }
    if (eq(key, "theme")) {
        // `auto` is not a theme, it is a SUBSCRIPTION: the app adopts the
        // light or dark member of the pair as the system reports it, and
        // re-adopts on every flip. The name written here is the one that is in
        // effect until the first appearance event lands — which the host emits
        // before the run loop arms, so in practice it is only ever the value
        // in the very first painted frame.
        if (theme_module.isAutoName(value)) {
            config.follow_system_theme = true;
            config.theme.set(theme_module.auto_dark) catch config.note(line, .too_long, value);
            return;
        }
        // A name this build does not ship is a `bad_value` and is NOT stored.
        // Storing it would leave `Config.theme` holding a string that resolves
        // to nothing, so every reader downstream would have to re-check
        // whether the name means anything — and the settings surface would
        // show a theme that does not exist as the one in effect.
        if (theme_module.byName(value) == null) {
            config.note(line, .bad_value, value);
            return;
        }
        config.theme.set(value) catch config.note(line, .too_long, value);
        return;
    }
    if (eq(key, "background")) return setColor(config, line, &config.background, value);
    if (eq(key, "foreground")) return setColor(config, line, &config.foreground, value);
    if (eq(key, "cursor-color")) return setColor(config, line, &config.cursor_color, value);
    if (eq(key, "selection-background")) return setColor(config, line, &config.selection_background, value);
    if (eq(key, "selection-foreground")) {
        // Parsed, but inert: canvas.TerminalGrid carries ONE selection_color
        // and the painter draws a WASH over the cell rather than overriding
        // the glyph's foreground. Honouring this needs an SDK field.
        //
        // Only ONE diagnostic per line: a malformed colour is a bad_value and
        // that is the more actionable of the two, so the unsupported note is
        // added only when the value itself was fine.
        setColor(config, line, &config.selection_foreground, value);
        if (config.selection_foreground != null) config.note(line, .unsupported_key, key);
        return;
    }

    if (eq(key, "cursor-style")) {
        config.cursor_style = CursorStyle.parse(value) orelse {
            config.note(line, .bad_value, value);
            return;
        };
        return;
    }
    if (eq(key, "minimum-contrast")) {
        const parsed = std.fmt.parseFloat(f32, value) catch {
            config.note(line, .bad_value, value);
            return;
        };
        // NaN is refused rather than clamped. `std.math.clamp` of a NaN is a
        // NaN, and a NaN floor compares false against every ratio, so it would
        // silently disable the floor while the config file says otherwise —
        // the worst of the three outcomes.
        if (std.math.isNan(parsed)) {
            config.note(line, .bad_value, value);
            return;
        }
        config.minimum_contrast = std.math.clamp(parsed, min_minimum_contrast, max_minimum_contrast);
        return;
    }
    if (eq(key, "cursor-style-blink")) return setBool(config, line, &config.cursor_style_blink, value);
    if (eq(key, "inherit-working-directory")) return setBool(config, line, &config.inherit_working_directory, value);
    if (eq(key, "hide-chrome-when-single")) return setBool(config, line, &config.hide_chrome_when_single, value);
    if (applyPhuxPair(config, line, key, value)) return;

    if (eq(key, "scrollback-limit")) {
        const parsed = std.fmt.parseInt(u64, value, 10) catch {
            config.note(line, .bad_value, value);
            return;
        };
        config.scrollback_bytes = @min(parsed, max_scrollback_bytes);
        return;
    }
    if (eq(key, "shell") or eq(key, "command")) {
        config.shell.set(value) catch config.note(line, .too_long, value);
        return;
    }
    if (eq(key, "tab-placement")) {
        config.tab_placement = TabPlacement.parse(value) orelse {
            config.note(line, .bad_value, value);
            return;
        };
        return;
    }
    if (eq(key, "window-padding")) {
        const parsed = std.fmt.parseFloat(f32, value) catch {
            config.note(line, .bad_value, value);
            return;
        };
        if (!std.math.isFinite(parsed) or parsed < 0 or parsed > 64) {
            config.note(line, .bad_value, value);
            return;
        }
        config.window_padding = parsed;
        return;
    }

    config.note(line, .unknown_key, key);
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

/// A `#` starts a comment ONLY at the start of a line (after any leading
/// whitespace). There are deliberately no trailing comments: `#` is also
/// how every color value begins, and a rule like "a `#` after whitespace
/// ends the line" silently eats `background = #1e1e2e`. Ghostty makes the
/// same call, so a config carried over from it behaves identically.
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

/// Parse bytes the caller read, or fall back to defaults when there were
/// none. A missing config is the normal case, not an error — file IO stays
/// with the caller so this module has no ambient dependency and stays
/// trivially testable.
pub fn loadOrDefault(bytes: ?[]const u8) Config {
    return parse(bytes orelse return .{});
}

// ------------------------------------------------------------------ writing

/// Rewrite a config file's bytes so that `key` reads `value`, touching as
/// little else as it possibly can.
///
/// THE RULE THIS FUNCTION EXISTS TO OBEY: this file is hand-written. Someone's
/// comments, their spacing, their ordering, and every key this build has never
/// heard of are all THEIRS, and an app that rewrites a person's file by
/// serializing its own parsed model back out would silently delete all of it —
/// including keys a NEWER build understands and this one does not. So the file
/// is edited as TEXT: every byte that is not the one line being set is copied
/// through verbatim, line terminators included.
///
/// The two things it does change, and why:
///
///   * a matching key's line is REPLACED IN PLACE, so the setting keeps the
///     position in the file its author gave it;
///   * a SECOND and later line naming the same key is DROPPED. The parser is
///     last-wins, so leaving a later duplicate would mean this function
///     returned successfully and the setting still did not take effect — a
///     silent no-op, which is the exact failure this whole round exists to
///     end. A duplicate key is already a bug in the file; collapsing it to the
///     one line that now holds the value is the honest repair.
///
/// A key that appears nowhere is APPENDED, with a newline first when the file
/// does not already end in one. Nothing else is added — no banner, no
/// "written by" comment, no reordering.
///
/// Comment lines are matched by the same rule the parser uses (`stripComment`:
/// a `#` in the first non-blank column), so a commented-out `# theme = nord`
/// is left exactly where it is and the live key is added separately. That is
/// the conservative reading: the user commented it out on purpose.
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
