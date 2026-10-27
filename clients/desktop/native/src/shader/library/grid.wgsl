// Grid: a striped sun setting over an endless neon floor. The floor rolls
// faster while agents work, and the sun flares when one needs you.

fn shade(uv: vec2f) -> vec4f {
    let aspect = phux.size.x / max(phux.size.y, 1.0);
    let horizon = 0.62;
    let p = vec2f(uv.x * aspect, uv.y);

    // Sky: a haze that thickens toward the horizon.
    let sky = smoothstep(0.05, horizon, uv.y) * step(uv.y, horizon);
    var color = mix(phux.ansi[5].rgb, phux.warning.rgb, sky * sky);
    var alpha = sky * sky * 0.3;

    // Sun: a gradient disc whose lower half is cut by scrolling stripes.
    let center = vec2f(0.5 * aspect, horizon - 0.13);
    let radius = 0.17;
    let d = distance(p, center);
    let below = (uv.y - center.y) / radius;
    let cut = step(0.0, below) * step(fract(below * 5.0 - phux.time * 0.25), below * 0.75);
    let disc = smoothstep(radius, radius - 0.004, d) * (1.0 - cut) * step(uv.y, horizon);
    let sun = mix(phux.warning.rgb, phux.danger.rgb, clamp((uv.y - center.y + radius) / (2.0 * radius), 0.0, 1.0));
    let halo = exp(-max(d - radius, 0.0) * 14.0) * (0.25 + 0.5 * phux.pulse) * step(uv.y, horizon);
    color = mix(color, sun, clamp(disc + halo, 0.0, 1.0));
    alpha = max(alpha, max(disc, halo));

    // Floor: perspective lines rolling toward you, fading into the haze.
    let depth = max(uv.y - horizon, 0.0001);
    let z = 0.18 / depth;
    let x = (uv.x - 0.5) * aspect * z;
    let roll = phux.time * (0.4 + 1.8 * phux.energy);
    // Distance to the nearest line in grid units, against a width of
    // a point and a half converted into grid units at this depth.
    let across = abs(fract(z * 2.0 + roll) - 0.5);
    let along = abs(fract(x * 1.2) - 0.5);
    let pixel = 1.5 / max(phux.size.y, 1.0);
    let across_width = clamp(pixel * 0.36 / (depth * depth), 0.001, 0.5);
    let along_width = clamp(pixel * 1.2 * z, 0.001, 0.5);
    let lines = max(
        smoothstep(across_width, 0.0, across),
        smoothstep(along_width, 0.0, along),
    );
    let ground = step(horizon, uv.y);
    let distance_fade = smoothstep(0.0, 0.25, depth);
    let floor_light = ground * (lines * (0.35 + 0.5 * distance_fade) + 0.08 * (1.0 - distance_fade));
    color = mix(color, phux.accent.rgb, clamp(floor_light * 2.0, 0.0, 1.0) * ground);
    alpha = max(alpha, clamp(floor_light, 0.0, 0.85));

    return vec4f(color, alpha * mix(0.75, 1.0, phux.dark));
}
