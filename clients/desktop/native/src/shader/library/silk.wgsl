// Silk: a slow mesh gradient in the theme's colors. Working agents stir it;
// one that needs you warms it in from the edges.

fn soften(color: vec4f) -> vec3f {
    return mix(phux.background.rgb, color.rgb, 0.92);
}

fn shade(uv: vec2f) -> vec4f {
    let aspect = phux.size.x / max(phux.size.y, 1.0);
    let t = phux.time * (0.05 + 0.12 * phux.energy);
    var p = vec2f(uv.x * aspect, uv.y);
    let warp = vec2f(
        phux_fbm(p * 1.3 + vec2f(t, -0.6 * t)),
        phux_fbm(p * 1.3 + vec2f(5.2 - 0.8 * t, 1.3 + t)),
    );
    p += (warp - 0.5) * 0.8;

    var anchors = array<vec2f, 4>(
        vec2f(0.1 * aspect + 0.15 * sin(t * 1.7), 0.15 + 0.1 * cos(t * 1.3)),
        vec2f(0.9 * aspect + 0.1 * cos(t * 1.1), 0.25 + 0.15 * sin(t * 0.9)),
        vec2f(0.3 * aspect + 0.2 * cos(t * 0.7), 0.9 + 0.08 * sin(t * 1.9)),
        vec2f(0.75 * aspect + 0.15 * sin(t * 1.3), 0.8 + 0.1 * cos(t * 1.5)),
    );
    var colors = array<vec3f, 4>(
        soften(phux.accent),
        soften(phux.ansi[5]),
        soften(phux.ansi[4]),
        soften(phux.ansi[6]),
    );
    var color = vec3f(0.0);
    var total = 0.0;
    for (var i = 0; i < 4; i++) {
        let d = distance(p, anchors[i]);
        let weight = 1.0 / (0.002 + d * d * d * d);
        color += colors[i] * weight;
        total += weight;
    }
    color /= total;

    let edge = smoothstep(0.3, 0.75, distance(uv, vec2f(0.5)));
    color = mix(color, phux.warning.rgb, phux.pulse * edge * 0.8);
    // Grain keeps the gradient from banding at eight bits.
    let grain = (phux_hash(uv * phux.size * phux.scale + fract(phux.time)) - 0.5) * 0.025;
    let alpha = mix(0.55, 0.7, phux.energy) + phux.pulse * edge * 0.3;
    return vec4f(color + grain, alpha * mix(0.75, 1.0, phux.dark));
}
