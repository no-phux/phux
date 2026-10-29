// Nebula: warped clouds of gas in the theme's blues and magentas, drifting
// toward the pointer. Working agents stir the gas; one that needs you lights
// its core amber.

fn shade(uv: vec2f) -> vec4f {
    let aspect = phux.size.x / max(phux.size.y, 1.0);
    let t = phux.time * (0.03 + 0.07 * phux.energy);
    var p = (uv - 0.5) * vec2f(aspect, 1.0) * 2.0;
    let inside = step(0.0, phux.pointer.x);
    p -= (phux.pointer - 0.5) * vec2f(aspect, 1.0) * 0.12 * inside;

    let q = vec2f(phux_fbm(p + vec2f(0.0, t)), phux_fbm(p + vec2f(5.2, 1.3 - t)));
    let r = vec2f(
        phux_fbm(p + 4.0 * q + vec2f(1.7, 9.2) + 0.15 * t),
        phux_fbm(p + 4.0 * q + vec2f(8.3, 2.8) - 0.12 * t),
    );
    let f = phux_fbm(p + 4.0 * r);

    var color = mix(phux.ansi[4].rgb, phux.accent.rgb, clamp(f * f * 2.2, 0.0, 1.0));
    color = mix(color, phux.ansi[5].rgb, clamp(length(q) - 0.4, 0.0, 1.0) * 0.7);
    color = mix(color, phux.foreground.rgb, clamp(r.x * r.x * r.x, 0.0, 1.0) * 0.35);
    let core = smoothstep(0.65, 0.95, f);
    color = mix(color, phux.warning.rgb, core * phux.pulse);

    let gas = smoothstep(0.3, 0.9, f) * (0.65 + 0.25 * phux.energy + 0.3 * phux.pulse * core);

    let cell = floor(uv * phux.size / 2.0);
    let twinkle = 0.5 + 0.5 * sin(phux.time * 2.3 + phux_hash(cell + 1.9) * 6.283);
    let star = step(0.997, phux_hash(cell)) * twinkle * (1.0 - gas);

    let alpha = clamp(max(gas, star * 0.7), 0.0, 0.85);
    color = mix(phux.foreground.rgb, color, gas / max(alpha, 0.001));
    return vec4f(color, alpha * mix(0.7, 1.0, phux.dark));
}
