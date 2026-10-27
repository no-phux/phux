// Cinema: a video behind your panes that dims while your agents work and
// comes back up when they stop. One that needs you flashes it bright and
// warms the edges. Copy this file into ~/.config/phux-desktop/shaders and
// point the next line at a file or a URL (YouTube needs yt-dlp installed).
//
// The media line takes a path (~ and paths relative to the shader folder
// work) or an http(s) URL.
// phux:media ~/Movies/backdrop.mp4

fn shade(uv: vec2f) -> vec4f {
    let aspect = phux.size.x / max(phux.size.y, 1.0);
    let centered = (uv - 0.5) * vec2f(aspect, 1.0);
    let vignette = 1.0 - smoothstep(0.35, 1.1, length(centered));

    // Until the first frame decodes, or with no video at all, a quiet glow.
    let glow = mix(phux.background.rgb, phux.accent.rgb, 0.25 * vignette);
    if phux.media < 0.5 {
        return vec4f(glow, 0.6);
    }

    var video = phux_media(phux_media_cover(uv)).rgb;
    // Working agents pull the picture down toward the theme: darker, a
    // little desaturated, tinted by the background.
    let luma = dot(video, vec3f(0.299, 0.587, 0.114));
    let calm = mix(video, vec3f(luma), 0.35 * phux.energy);
    let dim = mix(1.0, 0.3, phux.energy);
    var color = mix(phux.background.rgb, calm, dim);

    // The pointer peeks through the dimming.
    if phux.pointer.x >= 0.0 {
        let near = (phux.pointer - 0.5) * vec2f(aspect, 1.0);
        let peek = exp(-pow(length(centered - near) * 4.0, 2.0)) * phux.energy;
        color = mix(color, video, peek * 0.6);
    }

    // An agent that needs you: the picture flashes back and the edges warm.
    let edge = 1.0 - vignette;
    color = mix(color, video, phux.pulse);
    color = mix(color, phux.warning.rgb, phux.pulse * edge * 0.6);
    color *= mix(1.0, vignette, 0.45);

    let grain = (phux_hash(uv * phux.size * phux.scale + fract(phux.time)) - 0.5) * 0.02;
    let alpha = mix(0.95, 0.65, phux.energy) + phux.pulse * 0.05;
    return vec4f(color + grain, alpha);
}
