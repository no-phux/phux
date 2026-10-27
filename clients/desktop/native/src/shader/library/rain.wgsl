// Rain: glyphs falling in the accent color, the fleet playground's matrix
// map on your desk. More columns fall, and faster, while agents work; a
// blocked agent's columns burn amber, and a flash runs down the window when
// one starts needing you.

const CELL: f32 = 16.0;

fn glyph(cell: vec2f, local: vec2f, t: f32) -> f32 {
    let inner = (local - vec2f(0.18, 0.12)) / vec2f(0.64, 0.76);
    let inside = step(0.0, inner.x) * step(inner.x, 1.0) * step(0.0, inner.y) * step(inner.y, 1.0);
    let grid = clamp(inner, vec2f(0.0), vec2f(0.999)) * vec2f(3.0, 5.0);
    let roll = floor(t * (1.5 + 5.0 * phux_hash(cell + 0.5)));
    let lit = step(0.45, phux_hash(cell * 7.13 + floor(grid) * 1.37 + roll));
    let stroke = fract(grid);
    let solid = step(0.12, stroke.x) * step(stroke.x, 0.88) * step(0.1, stroke.y) * step(stroke.y, 0.9);
    return inside * lit * solid;
}

fn shade(uv: vec2f) -> vec4f {
    let points = uv * phux.size;
    let column = floor(points.x / CELL);
    let row = floor(points.y / CELL);
    let local = fract(points / CELL);

    let speed = (3.0 + 9.0 * phux_hash(vec2f(column, 1.0))) * (0.6 + 1.3 * phux.energy);
    let rows = phux.size.y / CELL;
    let span = rows + 10.0 + 24.0 * phux_hash(vec2f(column, 2.0));
    let head = (phux.time * speed + phux_hash(vec2f(column, 3.0)) * span) % span;
    let behind = head - row;
    let trail = 7.0 + 15.0 * phux_hash(vec2f(column, 4.0));
    let falling = step(phux_hash(vec2f(column, 5.0)), 0.3 + 0.6 * phux.energy);
    let in_trail = step(0.0, behind) * step(behind, trail);
    let fade = in_trail * pow(clamp(1.0 - behind / trail, 0.0, 1.0), 1.5) * falling;
    let lead = in_trail * step(behind, 1.0);

    let amber = step(phux_hash(vec2f(column, 6.0)), min(phux.blocked, 3.0) * 0.06);
    var color = mix(phux.accent.rgb, phux.warning.rgb, amber);
    color = mix(color, phux.foreground.rgb, lead * 0.75);

    let wave = phux.pulse * exp(-pow((uv.y - (1.0 - phux.pulse) * 1.2) * 7.0, 2.0));
    color = mix(color, phux.warning.rgb, wave);

    let ink = glyph(vec2f(column, row), local, phux.time);
    let alpha = clamp(ink * (fade * 0.85 + wave * 0.8) + fade * 0.05, 0.0, 0.9);
    return vec4f(color, alpha * mix(0.75, 1.0, phux.dark));
}
