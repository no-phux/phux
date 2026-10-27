---
audience: humans
stability: experimental
last-reviewed: 2026-10-27
---

# Backdrop shaders

**TL;DR.** **Settings > Appearance > Backdrop shader** puts a live WGSL shader
behind your panes and chrome, drawn in your theme's colors and moved by your
agents. Pick a built-in, or drop a `.wgsl` file in
`~/.config/phux-desktop/shaders` and save it to redraw the window.

## Choosing one

The **Backdrop shader** group shows a live card for **Off**, each built-in and
each file in your shader folder. Every card renders in the window's theme and
reacts to the window's agents, so it previews exactly what the window will
wear. Click a card to wear it; **Shader intensity** sets how strongly it shows
(70% by default). **Next Backdrop Shader** in the command palette and the
**View** menu cycles through the same list.

| Built-in   | What it draws                                                   |
| ---------- | --------------------------------------------------------------- |
| **Silk**   | A slow mesh gradient in the theme's accent and ANSI colors.     |
| **Aurora** | Curtains of light over faint stars.                             |
| **Nebula** | Warped clouds of gas and stars that lean toward the pointer.    |
| **Rain**   | Falling glyphs, the fleet playground's matrix map on your desk. |
| **Grid**   | A striped sun setting over a rolling neon floor.                |

The shader sits on the window's glass, under the sidebar, tabs and panes.
Panes keep their **Pane opacity**, so terminal text stays opaque and a lower
opacity shows more of the shader. The quick terminal and **Plain terminal**
never draw one. The choice is a display preference of the window, like its
theme.

## It moves with your agents

Every shader is fed the agents on every connected machine:

- **Energy** rises as agents start working and settles as they stop. The
  built-ins flow, fall and roll faster with it.
- **Pulse** flashes when one more agent starts needing you (blocked, finished
  or asking loudly) and fades over two seconds. The built-ins warm toward the
  theme's warning color; **Rain** sends a flash down the window and burns
  amber columns for blocked agents.

With **Animations** off, or macOS **Reduce motion** on, a shader shows one
still frame. It still follows the theme and the agent counts, but nothing
flashes.

## Writing your own

**New Shader** writes a commented starter, `my-backdrop.wgsl`, into the shader
folder, wears it and shows it in your file manager. **Show Folder** opens the folder.
Edit the file in any editor: the window reads it again within a second of a
save. A program that does not compile draws nothing; its line, column and
message appear under the cards and once in the notification feed.

A shader is one WGSL function:

```wgsl
fn shade(uv: vec2f) -> vec4f {
    let glow = exp(-length(uv - 0.5) * 3.0);
    return vec4f(phux.accent.rgb, glow * 0.5);
}
```

`uv` runs 0 to 1 from the top left of the window. Return straight (not
premultiplied) sRGB; alpha is how much of the window the shader covers, before
**Shader intensity**. Phux declares everything else, so a shader may not
declare bindings, entry points or names beginning with `phux` or `Phux`.

The `phux` uniform block:

| Field                                                                                                  | Type               | Meaning                                                                            |
| ------------------------------------------------------------------------------------------------------ | ------------------ | ---------------------------------------------------------------------------------- |
| `size`                                                                                                 | `vec2f`            | The window in points.                                                              |
| `pointer`                                                                                              | `vec2f`            | The pointer in `uv`, or `-1` when it is outside the window.                        |
| `time`, `frame`                                                                                        | `f32`              | Seconds and frames on the shader's clock. Reading either makes the shader animate. |
| `scale`                                                                                                | `f32`              | Rendered pixels per point.                                                         |
| `intensity`, `dark`                                                                                    | `f32`              | The slider; `1` for a dark theme, `0` for a light one.                             |
| `energy`, `pulse`                                                                                      | `f32`              | The fleet signals above, 0 to 1.                                                   |
| `working`, `blocked`, `done`, `idle`, `agents`                                                         | `f32`              | Agent counts.                                                                      |
| `background`, `foreground`, `surface`, `accent`, `cursor`, `selection`, `danger`, `warning`, `success` | `vec4f`            | The theme.                                                                         |
| `ansi`                                                                                                 | `array<vec4f, 16>` | The theme's terminal palette.                                                      |

`phux_hash(p)` and `phux_noise(p)` return 0 to 1 for a `vec2f`, and
`phux_fbm(p)` layers five octaves of noise.

A shader that never reads `phux.time` or `phux.frame` renders only when the
window's size, theme, intensity or agents change.

## Cost

Shaders render on the platform's own GPU API (Metal, Vulkan or Direct3D 12)
through wgpu, offscreen at one pixel per point, at most 1024 pixels on the
long side, and the window shows the result as an image. An animated shader
renders up to 30 frames a second while its window is key and 6 while it is
not, and only while the window is drawing frames: on macOS a covered,
hidden or minimized window renders nothing. Off, the default, opens no GPU device at all. A shader runs
on your GPU without a time limit: a loop that never ends can stall the
window's backdrop until the system resets the GPU.
