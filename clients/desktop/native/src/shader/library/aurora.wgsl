// Aurora: curtains of light in the accent and success colors over a faint
// field of stars. They flow faster while agents work and flare amber when
// one needs you.

fn curtain(uv: vec2f, aspect: f32, t: f32, base: f32, seed: f32) -> f32 {
    let x = uv.x * aspect;
    let wave = phux_fbm(vec2f(x * 0.8 + seed, t * 0.3 + seed));
    let edge = base + (wave - 0.5) * 0.4;
    let below = uv.y - edge;
    // A bright lower hem, rays fading upward from it.
    let body = select(exp(below * 7.0), exp(-below * below * 500.0), below > 0.0);
    let rays = 0.45 + 0.55 * phux_noise(vec2f(x * 30.0 + seed * 3.0, t * 0.7));
    return body * rays;
}

fn shade(uv: vec2f) -> vec4f {
    let aspect = phux.size.x / max(phux.size.y, 1.0);
    let t = phux.time * (0.25 + 0.7 * phux.energy);
    let near = curtain(uv, aspect, t, 0.46, 0.0);
    let far = curtain(uv, aspect, t * 0.8, 0.3, 7.3) * 0.65;

    let sweep = 0.5 + 0.5 * sin(uv.x * aspect * 2.2 + t * 0.4);
    let near_color = mix(phux.success.rgb, phux.accent.rgb, sweep);
    let far_color = mix(phux.accent.rgb, phux.ansi[5].rgb, 1.0 - sweep);
    let glow = near + far;
    var color = (near_color * near + far_color * far) / max(glow, 0.001);
    color = mix(color, phux.warning.rgb, phux.pulse * 0.85);

    let cell = floor(uv * phux.size / 2.5);
    let twinkle = 0.55 + 0.45 * sin(phux.time * 1.7 + phux_hash(cell + 3.1) * 6.283);
    let star = step(0.9965, phux_hash(cell)) * twinkle * (1.0 - smoothstep(0.0, 0.6, glow));

    let light = clamp(glow * (0.7 + 0.5 * phux.energy + 0.6 * phux.pulse), 0.0, 0.9);
    let alpha = max(light, star * 0.6);
    color = mix(phux.foreground.rgb, color, light / max(alpha, 0.001));
    return vec4f(color, alpha * mix(0.7, 1.0, phux.dark));
}
