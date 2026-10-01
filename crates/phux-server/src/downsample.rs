//! Per-client VT byte-stream rewriter (SPEC §6.2, ADR-0013).
//!
//! Raw PTY bytes are adapted to each client's capabilities: truecolor SGR
//! (semicolon or ITU colon form) is quantised to its [`ColorSupport`]; sixel,
//! kitty graphics, and iTerm2 images are dropped without the matching
//! [`ImageProtocol`]; kitty keyboard replies without `kbd_protocols` and
//! OSC 8 hyperlink framing without `hyperlinks` are stripped. Everything
//! else passes through byte for byte.

use phux_protocol::caps::{ClientCapabilities, ColorSupport, ImageProtocol, KeyboardProtocol};

/// Rewrite for the client's color tier only (other escapes pass through).
#[must_use]
pub fn rewrite_bytes(input: &[u8], support: ColorSupport) -> Vec<u8> {
    rewrite_bytes_with_caps(input, ClientCapabilities::new().with_color_support(support))
}

/// Whether the caps need no rewriting, so the source bytes can be forwarded
/// as-is.
#[must_use]
pub fn caps_pass_through(caps: ClientCapabilities) -> bool {
    matches!(caps.color_support, ColorSupport::TrueColor)
        && caps.image_protocols == phux_protocol::caps::ImageProtocolSet::all()
        && caps.kbd_protocols == phux_protocol::caps::KeyboardProtocolSet::all()
        && caps.hyperlinks
}

/// Rewrite an outbound VT stream for the full capability set (SPEC §6.2).
#[must_use]
pub fn rewrite_bytes_with_caps(input: &[u8], caps: ClientCapabilities) -> Vec<u8> {
    // Fast path: nothing to rewrite or drop. Hot path on capable clients.
    if caps_pass_through(caps) {
        return input.to_vec();
    }

    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] != ESC {
            // Most terminal output is plain text. Copy that run at once
            // rather than branching and pushing for every byte.
            let end = memchr::memchr(ESC, &input[i..]).map_or(input.len(), |offset| i + offset);
            out.extend_from_slice(&input[i..end]);
            i = end;
            continue;
        }
        // ESC at end of input — emit verbatim.
        if i + 1 >= input.len() {
            out.push(ESC);
            i += 1;
            continue;
        }
        match input[i + 1] {
            b'[' => {
                i = handle_csi(input, i, caps.color_support, &mut out);
            }
            b']' => {
                i = handle_osc(input, i, caps, &mut out);
            }
            b'P' => {
                i = handle_dcs(input, i, caps, &mut out);
            }
            b'_' => {
                i = handle_apc(input, i, caps, &mut out);
            }
            b'^' | b'X' => {
                // SOS / PM — pass through verbatim.
                i = passthrough_string_terminated(input, i, &mut out);
            }
            _ => {
                // Two-byte escape: ESC X.
                out.push(ESC);
                out.push(input[i + 1]);
                i += 2;
            }
        }
    }
    debug_assert!(
        out.len() <= input.len(),
        "capability adaptation must never expand its source"
    );
    out
}

/// ASCII escape (start of all VT control sequences).
const ESC: u8 = 0x1B;
/// ASCII bell / OSC string terminator.
const BEL: u8 = 0x07;

/// Handle a CSI at `input[start]`; returns the position past it. Only SGR
/// (`m`) is inspected. Any byte outside the parameter and intermediate
/// ranges ends the sequence, as real terminals treat malformed input.
fn handle_csi(input: &[u8], start: usize, support: ColorSupport, out: &mut Vec<u8>) -> usize {
    let csi_body_start = start + 2; // past ESC [
    let mut j = csi_body_start;
    while j < input.len() {
        let b = input[j];
        if (0x30..=0x3F).contains(&b) || (0x20..=0x2F).contains(&b) {
            j += 1;
            continue;
        }
        break;
    }
    if j >= input.len() {
        // Incomplete CSI; emit verbatim and stop.
        out.extend_from_slice(&input[start..]);
        return input.len();
    }
    let final_byte = input[j];
    if final_byte == b'm' {
        rewrite_sgr(&input[csi_body_start..j], support, out);
    } else {
        out.extend_from_slice(&input[start..=j]);
    }
    j + 1
}

/// Position past the `BEL` or `ESC \` terminator of the string sequence at
/// `start` (EOF if none).
const fn scan_string_terminated(input: &[u8], start: usize) -> usize {
    let mut j = start + 2;
    while j < input.len() {
        if input[j] == BEL {
            return j + 1;
        }
        if input[j] == ESC && j + 1 < input.len() && input[j + 1] == b'\\' {
            return j + 2;
        }
        j += 1;
    }
    input.len()
}

/// Pass an OSC/DCS/APC/SOS/PM sequence through verbatim. Returns the
/// new position past the string terminator.
fn passthrough_string_terminated(input: &[u8], start: usize, out: &mut Vec<u8>) -> usize {
    let end = scan_string_terminated(input, start);
    out.extend_from_slice(&input[start..end]);
    end
}

/// Handle `ESC ]` (OSC). Detects OSC 8 hyperlinks and OSC 1337 iTerm2
/// images; everything else passes through.
fn handle_osc(input: &[u8], start: usize, caps: ClientCapabilities, out: &mut Vec<u8>) -> usize {
    let end = scan_string_terminated(input, start);
    // Body lies between ESC ] and the terminator. Identify the OSC
    // command code (digits up to the first `;`).
    let body_start = start + 2;
    let mut p = body_start;
    while p < end && input[p].is_ascii_digit() {
        p += 1;
    }
    let code = &input[body_start..p];
    // OSC 8: strip only the framing; the linked text is outside it.
    if !caps.hyperlinks && code == b"8" {
        return end;
    }
    // OSC 1337 is gated on the code, not its subkey, as tmux does.
    if !caps.image_protocols.contains(ImageProtocol::Iterm2) && code == b"1337" {
        return end;
    }
    out.extend_from_slice(&input[start..end]);
    end
}

/// Handle DCS: drop sixel (introducer final `q`), pass the rest.
fn handle_dcs(input: &[u8], start: usize, caps: ClientCapabilities, out: &mut Vec<u8>) -> usize {
    let end = scan_string_terminated(input, start);
    if !caps.image_protocols.contains(ImageProtocol::Sixel) && is_sixel_dcs(&input[start + 2..end])
    {
        return end;
    }
    out.extend_from_slice(&input[start..end]);
    end
}

/// Whether a DCS body is a sixel introducer: parameters only, then `q`
/// (DECRQSS's `$ q` has an intermediate).
fn is_sixel_dcs(body: &[u8]) -> bool {
    let mut i = 0;
    while i < body.len() && (0x30..=0x3F).contains(&body[i]) {
        i += 1;
    }
    body.get(i).is_some_and(|b| *b == b'q')
}

/// Handle APC: `G` is kitty graphics, anything else a kitty keyboard reply;
/// each gated separately.
fn handle_apc(input: &[u8], start: usize, caps: ClientCapabilities, out: &mut Vec<u8>) -> usize {
    let end = scan_string_terminated(input, start);
    let payload_start = start + 2;
    let first = input.get(payload_start).copied();
    let is_graphics = first == Some(b'G');
    if is_graphics {
        if !caps.image_protocols.contains(ImageProtocol::KittyGraphics) {
            return end;
        }
    } else if !caps.kbd_protocols.contains(KeyboardProtocol::Kitty) {
        return end;
    }
    out.extend_from_slice(&input[start..end]);
    end
}

/// Rewrite SGR parameters into `CSI <rewritten> m`, handling both the
/// semicolon form and the ITU colon form (any colour-space id tolerated).
fn rewrite_sgr(params: &[u8], support: ColorSupport, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[");

    let mut first = true;
    let mut position = Some(0_usize);
    while let Some(start) = position {
        let (raw, next) = sgr_group(params, start);

        // ITU colon form: the entire truecolor spec lives in one group.
        if let Some(rgb) = parse_itu_truecolor(raw, 38) {
            emit_color_params(rgb, true, support, out, &mut first);
            position = next;
            continue;
        }
        if let Some(rgb) = parse_itu_truecolor(raw, 48) {
            emit_color_params(rgb, false, support, out, &mut first);
            position = next;
            continue;
        }

        // Semicolon form: inspect the next four groups without collecting
        // (no per-SGR allocation).
        if !raw.contains(&b':')
            && let Some(selector) = parse_single_param(raw)
            && matches!(selector, 38 | 48)
            && let Some((rgb, after)) = classic_truecolor(params, next)
        {
            emit_color_params(rgb, selector == 38, support, out, &mut first);
            position = after;
            continue;
        }

        // Other groups (including colon sub-parameters) pass verbatim.
        if !first {
            out.push(b';');
        }
        first = false;
        out.extend_from_slice(raw);
        position = next;
    }
    out.push(b'm');
}

fn sgr_group(params: &[u8], start: usize) -> (&[u8], Option<usize>) {
    let tail = &params[start..];
    tail.iter()
        .position(|byte| *byte == b';')
        .map_or((tail, None), |relative_end| {
            let end = start + relative_end;
            (&params[start..end], Some(end + 1))
        })
}

fn classic_truecolor(
    params: &[u8],
    mut position: Option<usize>,
) -> Option<([u8; 3], Option<usize>)> {
    let mut values = [0_u32; 4];
    for value in &mut values {
        let start = position?;
        let (raw, next) = sgr_group(params, start);
        *value = parse_single_param(raw)?;
        position = next;
    }
    if values[0] != 2 {
        return None;
    }
    Some((
        [
            clamp_u8(values[1]),
            clamp_u8(values[2]),
            clamp_u8(values[3]),
        ],
        position,
    ))
}

/// Parse a single colon-free SGR parameter group. Empty → `Some(0)`
/// per ECMA-48 default. Non-decimal → `None`.
fn parse_single_param(raw: &[u8]) -> Option<u32> {
    if raw.is_empty() {
        // An empty parameter means 0 for matching; output keeps the bytes.
        return Some(0);
    }
    let mut n: u32 = 0;
    for &b in raw {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.saturating_mul(10).saturating_add(u32::from(b - b'0'));
    }
    Some(n)
}

/// Parse an ITU colon-form truecolor group for `selector` (38/48):
/// `selector:2:R:G:B` or `selector:2:<space>:R:G:B`. `None` otherwise.
fn parse_itu_truecolor(raw: &[u8], selector: u32) -> Option<[u8; 3]> {
    if !raw.contains(&b':') {
        return None;
    }
    let fields: Vec<&[u8]> = raw.split(|b| *b == b':').collect();
    if parse_single_param(fields[0]) != Some(selector) {
        return None;
    }
    if fields.len() < 2 || parse_single_param(fields[1]) != Some(2) {
        return None;
    }
    let (ri, gi, bi) = match fields.len() {
        5 => (2, 3, 4),
        6 => (3, 4, 5),
        _ => return None,
    };
    let r = parse_single_param(fields[ri])?;
    let g = parse_single_param(fields[gi])?;
    let b = parse_single_param(fields[bi])?;
    Some([clamp_u8(r), clamp_u8(g), clamp_u8(b)])
}

/// Emit indexed-256 or indexed-16 color params, `;`-prefixed unless first.
fn emit_color_params(
    rgb: [u8; 3],
    foreground: bool,
    support: ColorSupport,
    out: &mut Vec<u8>,
    first: &mut bool,
) {
    if !*first {
        out.push(b';');
    }
    *first = false;
    match support {
        ColorSupport::TrueColor => {
            if foreground {
                out.extend_from_slice(b"38;2;");
            } else {
                out.extend_from_slice(b"48;2;");
            }
            write_decimal(u32::from(rgb[0]), out);
            out.push(b';');
            write_decimal(u32::from(rgb[1]), out);
            out.push(b';');
            write_decimal(u32::from(rgb[2]), out);
        }
        ColorSupport::Indexed256 => {
            let idx = nearest_xterm_256(rgb);
            if foreground {
                out.extend_from_slice(b"38;5;");
            } else {
                out.extend_from_slice(b"48;5;");
            }
            write_decimal(u32::from(idx), out);
        }
        ColorSupport::Indexed16 => {
            emit_indexed16(rgb, foreground, out);
        }
        // Unknown future tiers get the most restrictive treatment.
        _ => emit_indexed16(rgb, foreground, out),
    }
}

fn emit_indexed16(rgb: [u8; 3], foreground: bool, out: &mut Vec<u8>) {
    let idx = nearest_xterm_16(rgb);
    let base: u32 = if foreground {
        if idx < 8 { 30 } else { 90 }
    } else if idx < 8 {
        40
    } else {
        100
    };
    let off = u32::from(idx & 0x7);
    write_decimal(base + off, out);
}

/// Best-effort decimal writer that doesn't allocate.
fn write_decimal(n: u32, out: &mut Vec<u8>) {
    let mut buf = [0u8; 12];
    let mut i = buf.len();
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut v = n;
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + u8::try_from(v % 10).unwrap_or(0);
        v /= 10;
    }
    out.extend_from_slice(&buf[i..]);
}

/// Clamp a `u32` SGR parameter to `0..=255` for use as an RGB channel.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the `n > 255` guard bounds the cast"
)]
const fn clamp_u8(n: u32) -> u8 {
    if n > 255 { 255 } else { n as u8 }
}

// --- Palette tables ---

/// xterm 16-color system palette in RGB. Indices 0..=7 are the base ANSI
/// colors; 8..=15 are the bright variants.
const XTERM_16_PALETTE: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], //  0 black
    [0x80, 0x00, 0x00], //  1 red
    [0x00, 0x80, 0x00], //  2 green
    [0x80, 0x80, 0x00], //  3 yellow
    [0x00, 0x00, 0x80], //  4 blue
    [0x80, 0x00, 0x80], //  5 magenta
    [0x00, 0x80, 0x80], //  6 cyan
    [0xc0, 0xc0, 0xc0], //  7 white (light gray)
    [0x80, 0x80, 0x80], //  8 bright black (dark gray)
    [0xff, 0x00, 0x00], //  9 bright red
    [0x00, 0xff, 0x00], // 10 bright green
    [0xff, 0xff, 0x00], // 11 bright yellow
    [0x00, 0x00, 0xff], // 12 bright blue
    [0xff, 0x00, 0xff], // 13 bright magenta
    [0x00, 0xff, 0xff], // 14 bright cyan
    [0xff, 0xff, 0xff], // 15 bright white
];

/// xterm 6x6x6 color-cube step values.
const CUBE_STEPS: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];

#[inline]
const fn channel_to_cube_index(c: u8) -> u8 {
    if c < 48 {
        0
    } else if c < 115 {
        1
    } else if c < 155 {
        2
    } else if c < 195 {
        3
    } else if c < 235 {
        4
    } else {
        5
    }
}

#[inline]
const fn rgb_distance_sq(a: [u8; 3], b: [u8; 3]) -> u32 {
    let dr = a[0].abs_diff(b[0]) as u32;
    let dg = a[1].abs_diff(b[1]) as u32;
    let db = a[2].abs_diff(b[2]) as u32;
    dr * dr + dg * dg + db * db
}

#[derive(Debug, Clone, Copy)]
struct LabColor {
    l: f64,
    a: f64,
    b: f64,
}

fn srgb_channel_to_linear(c: u8) -> f64 {
    let v = f64::from(c) / 255.0;
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn lab_pivot(t: f64) -> f64 {
    const EPSILON: f64 = 216.0 / 24_389.0;
    const KAPPA: f64 = 24_389.0 / 27.0;

    if t > EPSILON {
        t.cbrt()
    } else {
        KAPPA.mul_add(t, 16.0) / 116.0
    }
}

fn rgb_to_lab(rgb: [u8; 3]) -> LabColor {
    let linear_red = srgb_channel_to_linear(rgb[0]);
    let linear_green = srgb_channel_to_linear(rgb[1]);
    let linear_blue = srgb_channel_to_linear(rgb[2]);

    // sRGB D65 conversion, normalized by the D65 reference white.
    let cie_x = 0.180_437_5_f64.mul_add(
        linear_blue,
        0.357_576_1_f64.mul_add(linear_green, 0.412_456_4 * linear_red),
    ) / 0.950_47;
    let cie_y = 0.072_175_f64.mul_add(
        linear_blue,
        0.715_152_2_f64.mul_add(linear_green, 0.212_672_9 * linear_red),
    );
    let cie_z = 0.950_304_1_f64.mul_add(
        linear_blue,
        0.119_192_f64.mul_add(linear_green, 0.019_333_9 * linear_red),
    ) / 1.088_83;

    let lab_x = lab_pivot(cie_x);
    let lab_y = lab_pivot(cie_y);
    let lab_z = lab_pivot(cie_z);

    LabColor {
        l: 116.0_f64.mul_add(lab_y, -16.0),
        a: 500.0 * (lab_x - lab_y),
        b: 200.0 * (lab_y - lab_z),
    }
}

fn lab_distance_sq(a: LabColor, b: LabColor) -> f64 {
    let dl = a.l - b.l;
    let da = a.a - b.a;
    let db = a.b - b.b;
    db.mul_add(db, dl.mul_add(dl, da * da))
}

fn nearest_xterm_256(rgb: [u8; 3]) -> u8 {
    let [r, g, b] = rgb;
    let cr = channel_to_cube_index(r);
    let cg = channel_to_cube_index(g);
    let cb = channel_to_cube_index(b);
    let cube_idx = 16 + 36 * cr + 6 * cg + cb;
    let cube_rgb = [
        CUBE_STEPS[cr as usize],
        CUBE_STEPS[cg as usize],
        CUBE_STEPS[cb as usize],
    ];
    let cube_d = rgb_distance_sq(rgb, cube_rgb);

    let avg_u16 = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    debug_assert!(avg_u16 <= 255);
    #[allow(clippy::cast_possible_truncation, reason = "avg_u16 <= 255")]
    let avg = avg_u16 as u8;
    let gray_idx_offset: u8 = if avg < 8 {
        0
    } else {
        let raw = (u16::from(avg) - 8 + 5) / 10;
        let clamped = raw.min(23);
        #[allow(clippy::cast_possible_truncation, reason = "<= 23")]
        let out = clamped as u8;
        out
    };
    let gray_idx = 232 + gray_idx_offset;
    let gray_byte = 8u8.saturating_add(gray_idx_offset.saturating_mul(10));
    let gray_d = rgb_distance_sq(rgb, [gray_byte, gray_byte, gray_byte]);

    if gray_d < cube_d { gray_idx } else { cube_idx }
}

fn nearest_xterm_16(rgb: [u8; 3]) -> u8 {
    let target = rgb_to_lab(rgb);
    let mut best_idx: u8 = 0;
    let mut best_d = f64::INFINITY;
    let mut i = 0u8;
    while i < 16 {
        let d = lab_distance_sq(target, rgb_to_lab(XTERM_16_PALETTE[usize::from(i)]));
        if d < best_d {
            best_d = d;
            best_idx = i;
        }
        i += 1;
    }
    best_idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::caps::{ImageProtocolSet, KeyboardProtocolSet};

    #[test]
    fn color_tier_rewrites() {
        use ColorSupport::{Indexed16 as I16, Indexed256 as I256, TrueColor as TC};
        let cases: &[(&[u8], ColorSupport, &[u8])] = &[
            (
                b"\x1b[38;2;255;0;0mhello\x1b[0m world",
                TC,
                b"\x1b[38;2;255;0;0mhello\x1b[0m world",
            ),
            (
                b"hello world\nplain ASCII\r\n",
                I16,
                b"hello world\nplain ASCII\r\n",
            ),
            (
                b"hello world\nplain ASCII\r\n",
                I256,
                b"hello world\nplain ASCII\r\n",
            ),
            (b"\x1b[38;2;255;0;0mX", I256, b"\x1b[38;5;196mX"),
            (b"\x1b[48;2;0;0;255mZ", I256, b"\x1b[48;5;21mZ"),
            (b"\x1b[38;2;255;0;0mX", I16, b"\x1b[91mX"),
            (b"\x1b[48;2;255;0;0mY", I16, b"\x1b[101mY"),
            (b"\x1b[38;2;128;0;0mX", I16, b"\x1b[31mX"),
            // Lab, not RGB distance: dark cyan is not black, dim blue is dim.
            (b"\x1b[38;2;0;64;64mX", I16, b"\x1b[36mX"),
            (b"\x1b[48;2;0;40;192mX", I16, b"\x1b[44mX"),
            (b"\x1b[1;38;2;255;0;0;4mX", I256, b"\x1b[1;38;5;196;4mX"),
            (
                b"\x1b[2J\x1b[Hhello\x1b[31m!",
                I16,
                b"\x1b[2J\x1b[Hhello\x1b[31m!",
            ),
            (
                b"\x1b]0;hello world\x1b\\rest",
                I16,
                b"\x1b]0;hello world\x1b\\rest",
            ),
            (b"\x1b]2;title\x07rest", I256, b"\x1b]2;title\x07rest"),
            (b"\x1b[mX", I256, b"\x1b[mX"),
            (b"abc\x1b", I256, b"abc\x1b"),
            (b"abc\x1b[38;2;1", I256, b"abc\x1b[38;2;1"),
            (b"\x1b=foo", I256, b"\x1b=foo"),
            // ITU colon forms: empty, explicit, and absent colour-space slot.
            (b"\x1b[38:2::255:0:0mX", I256, b"\x1b[38;5;196mX"),
            (b"\x1b[48:2::255:0:0mY", I16, b"\x1b[101mY"),
            (b"\x1b[38:2:0:255:0:0mX", I256, b"\x1b[38;5;196mX"),
            (b"\x1b[38:2:255:0:0mX", I256, b"\x1b[38;5;196mX"),
            (b"\x1b[38:2::255:0:0mX", TC, b"\x1b[38:2::255:0:0mX"),
            // Curly underline is not a color.
            (b"\x1b[4:3mX", I256, b"\x1b[4:3mX"),
        ];
        for (input, tier, want) in cases {
            assert_eq!(rewrite_bytes(input, *tier), *want, "{input:?} at {tier:?}");
        }
    }

    #[test]
    fn capability_gated_escapes() {
        let all = ClientCapabilities::new();
        let no_links = ClientCapabilities::new().with_hyperlinks(false);
        let no_images = ClientCapabilities::new().with_image_protocols(ImageProtocolSet::new());
        let no_kbd = ClientCapabilities::new().with_kbd_protocols(KeyboardProtocolSet::new());
        let no_kitty_graphics =
            ClientCapabilities::new().with_image_protocols(ImageProtocolSet::with(&[
                ImageProtocol::Sixel,
                ImageProtocol::Iterm2,
            ]));
        let osc8: &[u8] = b"\x1b]8;;https://example.com\x1b\\hello\x1b]8;;\x1b\\";
        let sixel: &[u8] = b"prefix\x1bP0;0;0q#0;2;100;0;0~~~\x1b\\suffix";
        let kitty_gfx: &[u8] = b"start\x1b_Ga=T,f=24;payload\x1b\\end";
        let iterm2: &[u8] = b"a\x1b]1337;File=name=test:AAAA\x1b\\b";
        let kbd: &[u8] = b"head\x1b_13;2u\x1b\\tail";
        let cases: &[(&[u8], ClientCapabilities, &[u8])] = &[
            (osc8, all, osc8),
            (osc8, no_links, b"hello"),
            (
                b"\x1b]8;;https://x.example\x07link text\x1b]8;;\x07trailing",
                no_links,
                b"link texttrailing",
            ),
            (
                b"\x1b]0;window title\x1b\\rest",
                no_links,
                b"\x1b]0;window title\x1b\\rest",
            ),
            (sixel, all, sixel),
            (sixel, no_images, b"prefixsuffix"),
            // DECRQSS (`$ q`) is not sixel.
            (b"\x1bP$q\"p\x1b\\", no_images, b"\x1bP$q\"p\x1b\\"),
            (kitty_gfx, all, kitty_gfx),
            (kitty_gfx, no_images, b"startend"),
            (iterm2, all, iterm2),
            (iterm2, no_images, b"ab"),
            (kbd, all, kbd),
            (kbd, no_kbd, b"headtail"),
            (b"x\x1b_Ga=T;abc\x1b\\y", no_kbd, b"x\x1b_Ga=T;abc\x1b\\y"),
            (
                b"x\x1b_13;2u\x1b\\y",
                no_kitty_graphics,
                b"x\x1b_13;2u\x1b\\y",
            ),
        ];
        for (i, (input, caps, want)) in cases.iter().enumerate() {
            assert_eq!(rewrite_bytes_with_caps(input, *caps), *want, "case {i}");
        }
    }

    #[test]
    fn plain_runs_preserve_binary_bytes_and_escape_boundaries() {
        let plain = b"text \xe6\x9d\xb1\xe4\xba\xac\x00\xff\x9b\r\n".repeat(80);
        let caps = ClientCapabilities::new()
            .with_color_support(ColorSupport::Indexed256)
            .with_image_protocols(ImageProtocolSet::new());
        for tail in [b"".as_slice(), b"\x1b", b"\x1b[38;2;255"] {
            let input = [
                plain.as_slice(),
                b"\x1b[38;2;255;0;0m",
                plain.as_slice(),
                b"\x1b_Ga=T;image\x1b\\",
                plain.as_slice(),
                tail,
            ]
            .concat();
            let expected = [
                plain.as_slice(),
                b"\x1b[38;5;196m",
                plain.as_slice(),
                plain.as_slice(),
                tail,
            ]
            .concat();
            assert_eq!(rewrite_bytes_with_caps(&input, caps), expected);
        }
    }

    #[test]
    fn color_downgrade_and_image_strip_compose() {
        let input = b"\x1b[38;2;255;0;0mhi\x1bP0;0;0qsixel\x1b\\done";
        let caps = ClientCapabilities::new()
            .with_color_support(ColorSupport::Indexed256)
            .with_image_protocols(ImageProtocolSet::with(&[
                ImageProtocol::KittyGraphics,
                ImageProtocol::Iterm2,
            ]));
        assert_eq!(
            rewrite_bytes_with_caps(input, caps),
            b"\x1b[38;5;196mhidone"
        );
    }
}
