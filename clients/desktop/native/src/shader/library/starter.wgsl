// A phux desktop backdrop. Save this file and the window redraws it; an
// error appears under Settings > Appearance > Backdrop with its line.
//
// Write one function, `shade`. `uv` runs 0..1 from the top left of the
// window; return straight (not premultiplied) sRGB with alpha. Alpha is how
// much of the backdrop covers the window, before the Intensity slider.
//
// The `phux` block carries:
//   size, pointer   vec2f  the window in points; the pointer in uv, or -1
//   time, frame     f32    seconds and frames (reading either animates)
//   scale           f32    rendered pixels per point
//   intensity, dark f32    the slider; 1 for a dark theme
//   energy          f32    0..1, eased: how busy your agents are
//   pulse           f32    1 when an agent starts needing you, fading to 0
//   working, blocked, done, idle, agents   f32   agent counts
//   background, foreground, surface, accent, cursor, selection,
//   danger, warning, success               vec4f  the theme
//   ansi            array<vec4f, 16>       the terminal palette
//   media, media_size f32, vec2f           1 while a video frame is bound
//
// A line starting `// phux:media` followed by a video file or URL plays it:
// phux_media(phux_media_cover(uv)) samples the frame, filling the window.
//
// Helpers: phux_hash(p) and phux_noise(p) return 0..1; phux_fbm(p) layers
// five octaves of noise. Names beginning with `phux` belong to phux.

fn shade(uv: vec2f) -> vec4f {
    let aspect = phux.size.x / max(phux.size.y, 1.0);
    let p = (uv - 0.5) * vec2f(aspect, 1.0);
    let breath = 0.5 + 0.5 * sin(phux.time * (0.6 + 2.0 * phux.energy));
    let glow = exp(-length(p) * (3.0 - breath));
    let color = mix(phux.accent.rgb, phux.warning.rgb, phux.pulse);
    return vec4f(color, glow * 0.5);
}
