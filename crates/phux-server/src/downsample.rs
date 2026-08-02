//! Per-client VT byte-stream rewriter (SPEC §6.2, [ADR-0013]).
//!
//! Under ADR-0013 the server forwards raw PTY bytes to each subscribed
//! client as `TERMINAL_OUTPUT` frames. Per SPEC §6.2 those bytes MUST be
//! adapted to the client's advertised capability set before forwarding:
//!
//! - Truecolor SGR (`CSI 38;2;R;G;B m` / `CSI 48;2;R;G;B m`) is quantised
//!   to the client's [`ColorSupport`] tier. ITU-style colon-separated
//!   form (`CSI 38:2::R:G:B m` per ECMA-48 §8.3.117) is recognised
//!   equivalently.
//! - Image-protocol escapes (sixel `DCS ... q ... ST`, kitty graphics
//!   `APC G ... ST`, iTerm2 `OSC 1337 ; ... ST`) are dropped when the
//!   client did not advertise the matching [`ImageProtocol`] bit.
//! - Kitty keyboard-protocol replies (`APC` without a leading `G`) are
//!   stripped when the client did not negotiate `kbd_protocols`. The
//!   server's canonical Terminal still processes them locally; this is
//!   wire-side filtering only.
//! - OSC 8 hyperlinks (`OSC 8 ; params ; URI ST`) are stripped when the
//!   client did not advertise `hyperlinks`. The intervening text between
//!   open and close hyperlinks is preserved — only the OSC bracketing
//!   bytes go.
//!
//! Every other escape (CSI for non-SGR finals, other OSCs, DCS, SOS/PM,
//! two-byte escapes) is passed through verbatim — phux is a multiplexer,
//! not a sanitiser, and unknown sequences must reach the client
//! byte-for-byte.
//!
//! [ADR-0013]: https://github.com/phall1/phux/blob/main/ADR/0013-libghostty-bytes-on-wire.md

use phux_protocol::caps::{ClientCapabilities, ColorSupport, ImageProtocol, KeyboardProtocol};

/// Rewrite an outbound VT byte stream to fit the client's color tier.
///
/// Thin wrapper over [`rewrite_bytes_with_caps`] preserved for existing
/// call sites that only care about color downsampling. Image, keyboard
/// and hyperlink escapes pass through unchanged.
#[must_use]
pub fn rewrite_bytes(input: &[u8], support: ColorSupport) -> Vec<u8> {
    rewrite_bytes_with_caps(input, ClientCapabilities::new().with_color_support(support))
}

/// `true` when the client's caps need no VT rewriting — the bytes can be
/// forwarded verbatim.
///
/// A truecolor client with every image, keyboard and hyperlink capability
/// permissive triggers no quantisation or escape dropping, so callers can
/// skip [`rewrite_bytes_with_caps`] entirely and forward the source buffer
/// (e.g. the refcounted broadcast [`bytes::Bytes`]) without a copy.
#[must_use]
pub fn caps_pass_through(caps: ClientCapabilities) -> bool {
    matches!(caps.color_support, ColorSupport::TrueColor)
        && caps.image_protocols == phux_protocol::caps::ImageProtocolSet::all()
        && caps.kbd_protocols == phux_protocol::caps::KeyboardProtocolSet::all()
        && caps.hyperlinks
}

/// Rewrite an outbound VT byte stream to fit the full client capability
/// set per SPEC §6.2.
///
/// The hot loop reuses a single output `Vec`; dropped sequences cost
/// nothing beyond the scan. Truecolor-only clients with everything
/// permissive get the fast path (single allocation + copy).
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
            out.push(input[i]);
            i += 1;
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

/// Handle a CSI sequence starting at `input[start] == ESC` with
/// `input[start + 1] == '['`. Returns the new position past the sequence.
///
/// Only `CSI ... m` (SGR) is structurally inspected; every other CSI
/// final byte passes through verbatim. The parameter byte set per
/// ECMA-48 is `0x30..=0x3F`; intermediates are `0x20..=0x2F`; the final
/// byte is `0x40..=0x7E`. We treat any byte outside the param/intermediate
/// range as the final byte, which matches what real terminals do on
/// truncated/malformed input.
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

/// Locate the string terminator (`BEL` or `ESC \\`) for the
/// OSC/DCS/APC/SOS/PM sequence starting at `input[start]`. Returns the
/// position one past the terminator. If no terminator is found the
/// sequence runs to EOF.
fn scan_string_terminated(input: &[u8], start: usize) -> usize {
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
    // OSC 8 hyperlinks. Strip framing only; the text between open and
    // close OSC 8 lives outside this sequence and is preserved by the
    // outer loop.
    if !caps.hyperlinks && code == b"8" {
        return end;
    }
    // OSC 1337 iTerm2 inline image. The protocol overloads OSC 1337
    // for non-image messages (e.g. shell integration) but per SPEC §6.2
    // the gating is on the OSC code, not the payload subkey — that
    // matches what tmux ships.
    if !caps.image_protocols.contains(ImageProtocol::Iterm2) && code == b"1337" {
        return end;
    }
    out.extend_from_slice(&input[start..end]);
    end
}

/// Handle `ESC P` (DCS). The sixel introducer is `DCS Pi ; Pa ; Ph q ...
/// ST` — the final `q` of the introducer is what marks it as sixel.
/// Other DCS strings (DECRQSS, tmux passthrough, etc.) pass through.
fn handle_dcs(input: &[u8], start: usize, caps: ClientCapabilities, out: &mut Vec<u8>) -> usize {
    let end = scan_string_terminated(input, start);
    if !caps.image_protocols.contains(ImageProtocol::Sixel) && is_sixel_dcs(&input[start + 2..end])
    {
        return end;
    }
    out.extend_from_slice(&input[start..end]);
    end
}

/// True when a DCS body (bytes past `ESC P`) is a sixel introducer.
///
/// Sixel uses `DCS Pa ; Pb ; Pc q ...` — parameter bytes only, no
/// intermediates, final `q`. DECRQSS also has `q` as final but with
/// `$` as an intermediate (`DCS $ q ... ST`); excluding intermediates
/// keeps DECRQSS, tmux passthrough, and other DCS payloads intact.
fn is_sixel_dcs(body: &[u8]) -> bool {
    let mut i = 0;
    while i < body.len() && (0x30..=0x3F).contains(&body[i]) {
        i += 1;
    }
    body.get(i).is_some_and(|b| *b == b'q')
}

/// Handle `ESC _` (APC). Kitty splits APC on the first payload byte:
/// `G` = graphics, otherwise it's a kitty keyboard protocol reply
/// (digits-and-semicolons-and-other-codes). Gate each independently.
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

/// Rewrite the parameter bytes of an SGR sequence (everything between
/// `CSI` and the terminating `m`), emitting `CSI <rewritten> m` into `out`.
///
/// Handles both the classic `;`-separated form (`CSI 38;2;R;G;B m`) and
/// the ITU/ECMA-48 §8.3.117 colon form (`CSI 38:2::R:G:B m`). The ITU
/// form reserves the field after the colour-space tag (`2`) for the
/// colour-space identifier; it is conventionally left empty and we
/// tolerate any value there.
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

        // Classic semicolon form: inspect the following four groups without
        // collecting every parameter into a heap Vec. Besides avoiding a
        // per-SGR allocation, this gives aggregate bootstrap adaptation the
        // exact peak bound of source bytes plus one output allocation.
        if !raw.contains(&b':')
            && let Some(selector) = parse_single_param(raw)
            && matches!(selector, 38 | 48)
            && let Some((rgb, after)) = classic_truecolor(params, next)
        {
            emit_color_params(rgb, selector == 38, support, out, &mut first);
            position = after;
            continue;
        }

        // Anything else passes through verbatim. The group's original bytes
        // (including any `:` sub-separators, e.g. `4:3` curly underline)
        // survive intact.
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
    match tail.iter().position(|byte| *byte == b';') {
        Some(relative_end) => {
            let end = start + relative_end;
            (&params[start..end], Some(end + 1))
        }
        None => (tail, None),
    }
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
        // ECMA-48: empty parameter defaults to 0. We surface that as 0
        // for matching purposes; emit-side path still respects the
        // original byte form so passthrough cases preserve emptiness.
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

/// Try to parse a single SGR group as an ITU colon-form truecolor spec
/// for `selector` (38 for fg, 48 for bg). Accepts:
///
/// - 5 fields: `selector:2:R:G:B` (some implementations skip the
///   colourspace slot).
/// - 6 fields: `selector:2:<colourspace>:R:G:B`. The colourspace field
///   is conventionally empty per ECMA-48 §8.3.117 but any value is
///   tolerated.
///
/// Returns `None` if the group lacks `:` (classic form lives at a
/// higher level), or if the selector / type-tag don't match.
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

/// Emit either `38;5;N` / `48;5;N` (Indexed256) or `3N` / `9N` /
/// `4N` / `10N` (Indexed16) into `out`, prefixing with `;` if `*first`
/// is false. Updates `*first` to false after emission.
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
        // `ColorSupport` is `#[non_exhaustive]`. Treat any future tier as
        // Indexed16 (most-restrictive) so we never accidentally forward
        // truecolor to a tier we don't yet model.
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

// -----------------------------------------------------------------------------
// Palette tables (preserved from the pre-ADR-0013 `phux_protocol::diff::cell`
// module, now relocated here because byte-stream rewriting is the only
// remaining consumer).
// -----------------------------------------------------------------------------

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

    fn caps_color(c: ColorSupport) -> ClientCapabilities {
        ClientCapabilities::new().with_color_support(c)
    }

    fn caps_strip_images() -> ClientCapabilities {
        ClientCapabilities::new().with_image_protocols(ImageProtocolSet::new())
    }

    #[test]
    fn truecolor_path_is_byte_identical() {
        let input = b"\x1b[38;2;255;0;0mhello\x1b[0m world";
        let out = rewrite_bytes(input, ColorSupport::TrueColor);
        assert_eq!(out, input);
    }

    #[test]
    fn ascii_passthrough_under_any_tier() {
        let input = b"hello world\nplain ASCII\r\n";
        for tier in [
            ColorSupport::TrueColor,
            ColorSupport::Indexed256,
            ColorSupport::Indexed16,
        ] {
            assert_eq!(rewrite_bytes(input, tier), input);
        }
    }

    #[test]
    fn truecolor_fg_to_256_red() {
        let input = b"\x1b[38;2;255;0;0mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, b"\x1b[38;5;196mX");
    }

    #[test]
    fn truecolor_bg_to_256_blue() {
        let input = b"\x1b[48;2;0;0;255mZ";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, b"\x1b[48;5;21mZ");
    }

    #[test]
    fn truecolor_fg_to_16_red_is_bright_red() {
        let input = b"\x1b[38;2;255;0;0mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed16);
        assert_eq!(out, b"\x1b[91mX");
    }

    #[test]
    fn truecolor_bg_to_16_red_is_bright_red_bg() {
        let input = b"\x1b[48;2;255;0;0mY";
        let out = rewrite_bytes(input, ColorSupport::Indexed16);
        assert_eq!(out, b"\x1b[101mY");
    }

    #[test]
    fn truecolor_fg_to_16_dark_red_is_dark_red() {
        let input = b"\x1b[38;2;128;0;0mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed16);
        assert_eq!(out, b"\x1b[31mX");
    }

    #[test]
    fn indexed16_uses_lab_for_dark_cyan_instead_of_black() {
        // RGB squared distance ties this with black; Lab keeps the visible hue.
        let input = b"\x1b[38;2;0;64;64mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed16);
        assert_eq!(out, b"\x1b[36mX");
    }

    #[test]
    fn indexed16_uses_lab_lightness_for_dim_blue() {
        // RGB squared distance prefers bright blue; Lab picks the dim ANSI blue.
        let input = b"\x1b[48;2;0;40;192mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed16);
        assert_eq!(out, b"\x1b[44mX");
    }

    #[test]
    fn mixed_sgr_parameters_partial_rewrite() {
        let input = b"\x1b[1;38;2;255;0;0;4mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, b"\x1b[1;38;5;196;4mX");
    }

    #[test]
    fn non_sgr_csi_passes_through() {
        let input = b"\x1b[2J\x1b[Hhello\x1b[31m!";
        let out = rewrite_bytes(input, ColorSupport::Indexed16);
        assert_eq!(out, input);
    }

    #[test]
    fn osc_sequences_pass_through() {
        let input = b"\x1b]0;hello world\x1b\\rest";
        let out = rewrite_bytes(input, ColorSupport::Indexed16);
        assert_eq!(out, input);
    }

    #[test]
    fn osc_bell_terminated_passes_through() {
        let input = b"\x1b]2;title\x07rest";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, input);
    }

    #[test]
    fn empty_sgr_is_preserved() {
        let input = b"\x1b[mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, b"\x1b[mX");
    }

    #[test]
    fn lone_esc_at_eof_passes_through() {
        let input = b"abc\x1b";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, input);
    }

    #[test]
    fn truncated_csi_at_eof_passes_through() {
        let input = b"abc\x1b[38;2;1";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, input);
    }

    #[test]
    fn two_byte_escape_passes_through() {
        let input = b"\x1b=foo";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, input);
    }

    // --- ITU colon SGR (phux-9gz) -----------------------------------------

    #[test]
    fn itu_colon_truecolor_fg_downgrades_to_256() {
        // CSI 38:2::255:0:0 m → indexed256 red 196.
        // The empty middle field is the ECMA-48 colourspace slot.
        let input = b"\x1b[38:2::255:0:0mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, b"\x1b[38;5;196mX");
    }

    #[test]
    fn itu_colon_truecolor_bg_downgrades_to_16() {
        let input = b"\x1b[48:2::255:0:0mY";
        let out = rewrite_bytes(input, ColorSupport::Indexed16);
        assert_eq!(out, b"\x1b[101mY");
    }

    #[test]
    fn itu_colon_truecolor_with_explicit_colourspace_is_tolerated() {
        // Some emitters fill the colourspace slot with `0` rather than
        // leaving it empty. Tolerate per ECMA-48 §8.3.117.
        let input = b"\x1b[38:2:0:255:0:0mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, b"\x1b[38;5;196mX");
    }

    #[test]
    fn itu_colon_5field_form_also_works() {
        // Some implementations emit 38:2:R:G:B (no colourspace slot).
        let input = b"\x1b[38:2:255:0:0mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, b"\x1b[38;5;196mX");
    }

    #[test]
    fn itu_colon_truecolor_passthrough_under_truecolor_client() {
        // Under TrueColor + everything-permissive, the fast path returns
        // the input bytewise — including the ITU form intact.
        let input = b"\x1b[38:2::255:0:0mX";
        let out = rewrite_bytes(input, ColorSupport::TrueColor);
        assert_eq!(out, input);
    }

    #[test]
    fn non_color_colon_sgr_passes_through_verbatim() {
        // `4:3` is the curly-underline SGR. Must survive verbatim — the
        // rewriter only recognises color sub-sequences.
        let input = b"\x1b[4:3mX";
        let out = rewrite_bytes(input, ColorSupport::Indexed256);
        assert_eq!(out, input);
    }

    // --- OSC 8 hyperlinks (phux-9gz) --------------------------------------

    #[test]
    fn osc8_open_close_passthrough_when_hyperlinks_allowed() {
        // OSC 8 ; ; https://example.com ST  hello  OSC 8 ; ; ST
        let input = b"\x1b]8;;https://example.com\x1b\\hello\x1b]8;;\x1b\\";
        let out = rewrite_bytes_with_caps(input, ClientCapabilities::new());
        assert_eq!(out, input);
    }

    #[test]
    fn osc8_stripped_when_hyperlinks_disabled() {
        let input = b"\x1b]8;;https://example.com\x1b\\hello\x1b]8;;\x1b\\";
        let caps = ClientCapabilities::new().with_hyperlinks(false);
        let out = rewrite_bytes_with_caps(input, caps);
        // Only the inner `hello` survives.
        assert_eq!(out, b"hello");
    }

    #[test]
    fn osc8_with_bel_terminator_also_stripped() {
        // Some emitters use BEL instead of ST.
        let input = b"\x1b]8;;https://x.example\x07link text\x1b]8;;\x07trailing";
        let caps = ClientCapabilities::new().with_hyperlinks(false);
        let out = rewrite_bytes_with_caps(input, caps);
        assert_eq!(out, b"link texttrailing");
    }

    #[test]
    fn osc_non_8_unaffected_by_hyperlink_strip() {
        // Window title (OSC 0) must NOT be stripped when hyperlinks=false.
        let input = b"\x1b]0;window title\x1b\\rest";
        let caps = ClientCapabilities::new().with_hyperlinks(false);
        let out = rewrite_bytes_with_caps(input, caps);
        assert_eq!(out, input);
    }

    // --- Image protocols (phux-9gz) ---------------------------------------

    #[test]
    fn sixel_passthrough_when_bit_set() {
        // DCS 0 ; 0 ; 0 q ... ST — minimal sixel body.
        let input = b"prefix\x1bP0;0;0q#0;2;100;0;0~~~\x1b\\suffix";
        let out = rewrite_bytes_with_caps(input, ClientCapabilities::new());
        assert_eq!(out, input);
    }

    #[test]
    fn sixel_dropped_when_bit_unset() {
        let input = b"prefix\x1bP0;0;0q#0;2;100;0;0~~~\x1b\\suffix";
        let out = rewrite_bytes_with_caps(input, caps_strip_images());
        assert_eq!(out, b"prefixsuffix");
    }

    #[test]
    fn non_sixel_dcs_survives_image_strip() {
        // DECRQSS-style DCS (final `|`, not `q`) must not be dropped
        // when only sixel is disabled.
        let input = b"\x1bP$q\"p\x1b\\";
        let out = rewrite_bytes_with_caps(input, caps_strip_images());
        assert_eq!(out, input);
    }

    #[test]
    fn kitty_graphics_passthrough_when_bit_set() {
        // APC G a=T,f=24;<payload> ST
        let input = b"start\x1b_Ga=T,f=24;payload\x1b\\end";
        let out = rewrite_bytes_with_caps(input, ClientCapabilities::new());
        assert_eq!(out, input);
    }

    #[test]
    fn kitty_graphics_dropped_when_bit_unset() {
        let input = b"start\x1b_Ga=T,f=24;payload\x1b\\end";
        let out = rewrite_bytes_with_caps(input, caps_strip_images());
        assert_eq!(out, b"startend");
    }

    #[test]
    fn iterm2_image_dropped_when_bit_unset() {
        // OSC 1337 ; File=name=...:base64 ST
        let input = b"a\x1b]1337;File=name=test:AAAA\x1b\\b";
        let out = rewrite_bytes_with_caps(input, caps_strip_images());
        assert_eq!(out, b"ab");
    }

    #[test]
    fn iterm2_image_passthrough_when_bit_set() {
        let input = b"a\x1b]1337;File=name=test:AAAA\x1b\\b";
        let out = rewrite_bytes_with_caps(input, ClientCapabilities::new());
        assert_eq!(out, input);
    }

    // --- Kitty keyboard protocol APC (phux-9gz) ---------------------------

    #[test]
    fn kitty_kbd_reply_stripped_when_disabled() {
        // APC without leading `G` — kitty kbd protocol payload.
        let input = b"head\x1b_13;2u\x1b\\tail";
        let caps = ClientCapabilities::new().with_kbd_protocols(KeyboardProtocolSet::new());
        let out = rewrite_bytes_with_caps(input, caps);
        assert_eq!(out, b"headtail");
    }

    #[test]
    fn kitty_kbd_reply_survives_when_enabled() {
        let input = b"head\x1b_13;2u\x1b\\tail";
        let out = rewrite_bytes_with_caps(input, ClientCapabilities::new());
        assert_eq!(out, input);
    }

    #[test]
    fn kitty_graphics_not_stripped_by_kbd_disable() {
        // Disabling kbd_protocols must not affect APC `G` graphics
        // payloads (they are gated independently).
        let input = b"x\x1b_Ga=T;abc\x1b\\y";
        let caps = ClientCapabilities::new().with_kbd_protocols(KeyboardProtocolSet::new());
        let out = rewrite_bytes_with_caps(input, caps);
        assert_eq!(out, input);
    }

    #[test]
    fn kitty_kbd_not_stripped_by_graphics_disable() {
        // Conversely, disabling kitty_graphics must not eat kbd replies.
        let input = b"x\x1b_13;2u\x1b\\y";
        let caps = ClientCapabilities::new().with_image_protocols(ImageProtocolSet::with(&[
            ImageProtocol::Sixel,
            ImageProtocol::Iterm2,
        ]));
        let out = rewrite_bytes_with_caps(input, caps);
        assert_eq!(out, input);
    }

    // --- Composition ------------------------------------------------------

    #[test]
    fn caps_with_color_downgrade_and_image_strip_compose() {
        // Truecolor SGR + sixel inside the same stream; client wants
        // Indexed256 and refuses sixel.
        let input = b"\x1b[38;2;255;0;0mhi\x1bP0;0;0qsixel\x1b\\done";
        let caps =
            caps_color(ColorSupport::Indexed256).with_image_protocols(ImageProtocolSet::with(&[
                ImageProtocol::KittyGraphics,
                ImageProtocol::Iterm2,
            ]));
        let out = rewrite_bytes_with_caps(input, caps);
        assert_eq!(out, b"\x1b[38;5;196mhidone");
    }

    #[test]
    fn caps_color_function_smoke() {
        // Smoke-check that the convenience helper produces a permissive
        // base with just color toggled.
        let c = caps_color(ColorSupport::Indexed16);
        assert!(c.image_protocols.contains(ImageProtocol::Sixel));
        assert!(c.hyperlinks);
        assert!(matches!(c.color_support, ColorSupport::Indexed16));
    }
}
