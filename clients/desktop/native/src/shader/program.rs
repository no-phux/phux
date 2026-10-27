//! The backdrop shader contract.
//!
//! A program is one WGSL function, `fn shade(uv: vec2f) -> vec4f`: `uv` runs
//! 0..1 from the top left, and the result is straight (not premultiplied)
//! sRGB with alpha. Phux declares the `phux` uniform block above it and the
//! entry points below it, so a program may not declare bindings or entry
//! points of its own. Errors are reported against the program's own lines.

use std::hash::{Hash, Hasher};

/// Largest program accepted, in bytes.
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;

/// Declared above every program. [`Uniforms::pack`] writes this layout and a
/// test checks it against naga's own offsets.
const PRELUDE: &str = "\
struct Phux {
    size: vec2f,
    pointer: vec2f,
    time: f32,
    scale: f32,
    intensity: f32,
    dark: f32,
    energy: f32,
    pulse: f32,
    working: f32,
    blocked: f32,
    done: f32,
    idle: f32,
    agents: f32,
    frame: f32,
    background: vec4f,
    foreground: vec4f,
    surface: vec4f,
    accent: vec4f,
    cursor: vec4f,
    selection: vec4f,
    danger: vec4f,
    warning: vec4f,
    success: vec4f,
    ansi: array<vec4f, 16>,
}
@group(0) @binding(0) var<uniform> phux: Phux;
fn phux_hash(p: vec2f) -> f32 {
    var q = fract(p * vec2f(123.34, 456.21));
    q += dot(q, q + 45.32);
    return fract(q.x * q.y);
}
fn phux_noise(p: vec2f) -> f32 {
    let cell = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = phux_hash(cell);
    let b = phux_hash(cell + vec2f(1.0, 0.0));
    let c = phux_hash(cell + vec2f(0.0, 1.0));
    let d = phux_hash(cell + vec2f(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}
fn phux_fbm(p: vec2f) -> f32 {
    var value = 0.0;
    var amplitude = 0.5;
    var q = p;
    for (var octave = 0; octave < 5; octave++) {
        value += amplitude * phux_noise(q);
        q = q * 2.03 + vec2f(17.1, 9.2);
        amplitude *= 0.5;
    }
    return value;
}
";

/// Appended after every program: one triangle covering the target, and the
/// fragment stage that calls `shade` and applies the intensity.
const POSTLUDE: &str = "
struct PhuxVarying {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}
@vertex fn phux_vertex(@builtin(vertex_index) index: u32) -> PhuxVarying {
    let corner = vec2f(f32((index << 1u) & 2u), f32(index & 2u));
    var out: PhuxVarying;
    out.position = vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2f(corner.x, 1.0 - corner.y);
    return out;
}
@fragment fn phux_fragment(phux_in: PhuxVarying) -> @location(0) vec4f {
    let color = clamp(shade(phux_in.uv), vec4f(0.0), vec4f(1.0));
    return vec4f(color.rgb, color.a * phux.intensity);
}
";

pub const VERTEX_ENTRY: &str = "phux_vertex";
pub const FRAGMENT_ENTRY: &str = "phux_fragment";

/// Size of the `phux` block in bytes.
pub const UNIFORM_BYTES: usize = 464;

/// Why a program was refused, against the program's own source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderError {
    /// 1-based line and column in the program, when the error has a place.
    pub at: Option<(u32, u32)>,
    pub message: String,
}

impl ShaderError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            at: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.at {
            Some((line, column)) => write!(formatter, "{line}:{column}: {}", self.message),
            None => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for ShaderError {}

/// A checked program, ready for any renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// Shown in errors and Settings.
    pub name: String,
    /// The complete module: prelude, program, entry points.
    pub wgsl: String,
    /// Reads `phux.time` or `phux.frame`, so it wants frames on a clock.
    pub animated: bool,
    /// Identity of `wgsl`, for pipeline caches.
    pub hash: u64,
}

impl Program {
    /// Check `source` and wrap it in the contract.
    pub fn compile(name: &str, source: &str) -> Result<Self, ShaderError> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err(ShaderError::new(format!(
                "the shader is {} KiB; the limit is {} KiB",
                source.len() / 1024,
                MAX_SOURCE_BYTES / 1024
            )));
        }
        let wgsl = format!("{PRELUDE}{source}{POSTLUDE}");
        let program_lines = line_count(source);
        let place = |location: Option<naga::SourceLocation>| {
            located(location, line_count(PRELUDE), program_lines)
        };
        let module = naga::front::wgsl::parse_str(&wgsl)
            .map_err(|error| explain(place(error.location(&wgsl)), error.message().to_owned()))?;
        sandbox(&module)?;
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .map_err(|error| {
            explain(
                place(validation_location(&error, &wgsl)),
                validation_message(&error),
            )
        })?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        wgsl.hash(&mut hasher);
        Ok(Self {
            name: name.to_owned(),
            animated: reads_clock(source),
            hash: hasher.finish(),
            wgsl,
        })
    }
}

/// Where an error in the whole module falls. An error in the contract's own
/// lines means the program broke it: no `shade`, a mistyped one, or a name
/// the contract uses.
enum Place {
    Program(u32, u32),
    Contract,
    Nowhere,
}

fn located(location: Option<naga::SourceLocation>, prelude: u32, program: u32) -> Place {
    let Some(location) = location else {
        return Place::Nowhere;
    };
    let line = location.line_number;
    if line > prelude && line <= prelude + program.max(1) {
        Place::Program(line - prelude, location.line_position)
    } else {
        Place::Contract
    }
}

fn explain(place: Place, message: String) -> ShaderError {
    match place {
        Place::Program(line, column) => ShaderError {
            at: Some((line, column)),
            message,
        },
        Place::Contract => ShaderError::new(format!(
            "define `fn shade(uv: vec2f) -> vec4f`, and leave names beginning with \
             `phux` or `Phux` to phux ({message})"
        )),
        Place::Nowhere => ShaderError::new(message),
    }
}

/// The most specific cause of a validation error, with naga's handles and
/// type dumps rewritten as WGSL.
fn validation_message(error: &naga::WithSpan<naga::valid::ValidationError>) -> String {
    let mut innermost: &dyn std::error::Error = error.as_inner();
    while let Some(cause) = innermost.source() {
        innermost = cause;
    }
    tidy(&innermost.to_string())
}

/// The most specific place a validation error names: naga lists the
/// enclosing function first and the offending expression last.
fn validation_location(
    error: &naga::WithSpan<naga::valid::ValidationError>,
    source: &str,
) -> Option<naga::SourceLocation> {
    error
        .spans()
        .filter(|(span, _)| span.is_defined())
        .last()
        .map(|(span, _)| span.location(source))
}

fn tidy(message: &str) -> String {
    let mut text = message.to_owned();
    for (kind, short, width) in [
        ("Float", "f", 4),
        ("Sint", "i", 4),
        ("Uint", "u", 4),
        ("Bool", "bool", 1),
    ] {
        let scalar = format!("Scalar {{ kind: {kind}, width: {width} }}");
        for (size, count) in [("Bi", 2), ("Tri", 3), ("Quad", 4)] {
            let vector = format!("Vector {{ size: {size}, scalar: {scalar} }}");
            let named = if kind == "Bool" {
                format!("vec{count}<bool>")
            } else {
                format!("vec{count}{short}")
            };
            text = text.replace(&vector, &named);
        }
        let named = if kind == "Bool" {
            "bool".to_owned()
        } else {
            format!("{short}32")
        };
        text = text.replace(&format!("Scalar({scalar})"), &named);
        text = text.replace(&scalar, &named);
    }
    // `[3] (of type vec4f)` reads as `vec4f`.
    let mut tidy = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(open) = rest.find('[') {
        let (before, from) = rest.split_at(open);
        tidy.push_str(before);
        let digits = from[1..].find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
        let after = &from[1 + digits..];
        if digits > 0
            && let Some(typed) = after.strip_prefix("] (of type ")
            && let Some(close) = closing_paren(typed)
        {
            tidy.push_str(&typed[..close]);
            rest = &typed[close + 1..];
            continue;
        }
        if digits > 0
            && let Some(plain) = after.strip_prefix("] ")
        {
            rest = plain;
            continue;
        }
        tidy.push('[');
        rest = &from[1..];
    }
    tidy.push_str(rest);
    tidy
}

/// The program brings only functions, constants and private state: the
/// contract owns every binding and entry point.
fn sandbox(module: &naga::Module) -> Result<(), ShaderError> {
    if let Some(extra) = module
        .entry_points
        .iter()
        .map(|entry| entry.name.as_str())
        .find(|name| *name != VERTEX_ENTRY && *name != FRAGMENT_ENTRY)
    {
        return Err(ShaderError::new(format!(
            "`{extra}` is an entry point; phux provides them, so write only `fn shade`"
        )));
    }
    let bound = module
        .global_variables
        .iter()
        .filter(|(_, global)| global.binding.is_some())
        .filter_map(|(_, global)| global.name.as_deref())
        .find(|name| *name != "phux");
    if let Some(name) = bound {
        return Err(ShaderError::new(format!(
            "`{name}` declares a binding; a shader reads only `phux`"
        )));
    }
    let workgroup = module
        .global_variables
        .iter()
        .any(|(_, global)| matches!(global.space, naga::AddressSpace::WorkGroup));
    if workgroup {
        return Err(ShaderError::new("workgroup variables are not available"));
    }
    Ok(())
}

/// The `)` that closes a parenthesis already open before `text`.
fn closing_paren(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (at, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(at),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn line_count(text: &str) -> u32 {
    u32::try_from(text.lines().count()).unwrap_or(u32::MAX)
}

/// Whether the source, outside comments, reads the clock.
fn reads_clock(source: &str) -> bool {
    let code = strip_comments(source);
    ["time", "frame"].iter().any(|field| {
        code.match_indices("phux")
            .any(|(at, _)| member_after(&code[at + 4..]) == Some(*field))
    })
}

/// `.name` after optional whitespace.
fn member_after(rest: &str) -> Option<&str> {
    let rest = rest.trim_start().strip_prefix('.')?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

fn strip_comments(source: &str) -> String {
    let mut code = String::with_capacity(source.len());
    let mut rest = source;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.find('\n').map_or("", |end| &after[end..]);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |end| &after[end + 2..]);
            code.push(' ');
        } else {
            let next = rest.chars().next().unwrap_or(' ');
            code.push(next);
            rest = &rest[next.len_utf8()..];
        }
    }
    code
}

/// Straight sRGB, 0..1.
pub type Rgba = [f32; 4];

/// Everything the `phux` block carries for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Uniforms {
    /// The surface in points.
    pub size: [f32; 2],
    /// The pointer in uv, or `[-1, -1]` when it is not over the window.
    pub pointer: [f32; 2],
    /// Seconds on the surface's clock.
    pub time: f32,
    /// Rendered pixels per point.
    pub scale: f32,
    /// How strongly the backdrop shows, 0..1; multiplies the result's alpha.
    pub intensity: f32,
    /// 1 for a dark theme, 0 for a light one.
    pub dark: f32,
    /// How busy the fleet is, eased: 0 idle, toward 1 with many agents working.
    pub energy: f32,
    /// 1 the moment an agent starts needing you, decaying to 0 in about two seconds.
    pub pulse: f32,
    pub working: f32,
    pub blocked: f32,
    pub done: f32,
    pub idle: f32,
    pub agents: f32,
    /// Frames rendered on this surface.
    pub frame: f32,
    pub background: Rgba,
    pub foreground: Rgba,
    pub surface: Rgba,
    pub accent: Rgba,
    pub cursor: Rgba,
    pub selection: Rgba,
    pub danger: Rgba,
    pub warning: Rgba,
    pub success: Rgba,
    pub ansi: [Rgba; 16],
}

impl Default for Uniforms {
    fn default() -> Self {
        let gray = |value: f32| [value, value, value, 1.];
        Self {
            size: [1., 1.],
            pointer: [-1., -1.],
            time: 0.,
            scale: 1.,
            intensity: 1.,
            dark: 1.,
            energy: 0.,
            pulse: 0.,
            working: 0.,
            blocked: 0.,
            done: 0.,
            idle: 0.,
            agents: 0.,
            frame: 0.,
            background: gray(0.08),
            foreground: gray(0.9),
            surface: gray(0.12),
            accent: [0.45, 0.6, 1., 1.],
            cursor: gray(0.9),
            selection: gray(0.3),
            danger: [0.95, 0.35, 0.35, 1.],
            warning: [0.95, 0.75, 0.3, 1.],
            success: [0.4, 0.85, 0.5, 1.],
            ansi: [gray(0.5); 16],
        }
    }
}

impl Uniforms {
    /// The block in WGSL's uniform layout, native-endian as the GPU reads it.
    #[must_use]
    pub fn pack(&self) -> [u8; UNIFORM_BYTES] {
        let mut floats = Vec::with_capacity(UNIFORM_BYTES / 4);
        floats.extend_from_slice(&self.size);
        floats.extend_from_slice(&self.pointer);
        floats.extend_from_slice(&[
            self.time,
            self.scale,
            self.intensity,
            self.dark,
            self.energy,
            self.pulse,
            self.working,
            self.blocked,
            self.done,
            self.idle,
            self.agents,
            self.frame,
        ]);
        for color in [
            self.background,
            self.foreground,
            self.surface,
            self.accent,
            self.cursor,
            self.selection,
            self.danger,
            self.warning,
            self.success,
        ]
        .iter()
        .chain(&self.ansi)
        {
            floats.extend_from_slice(color);
        }
        let mut bytes = [0; UNIFORM_BYTES];
        for (slot, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(floats) {
            slot.copy_from_slice(&value.to_ne_bytes());
        }
        bytes
    }
}

#[cfg(test)]
mod tests;
