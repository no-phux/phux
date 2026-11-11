# Mirror formats and platforms

The frame formats of the surface and page mirrors, how the tree and
surface mirrors run on Linux, and what the host shows while input arrives. Part of [resource kinds beyond terminals](./resource-kinds.md).

A surface frame is a PNG or a JPEG, told apart by its signature; the codec
keeps its `PixelSurfacePngV1` name. `phux agent surface mirror` and `phux
agent page mirror` take `--format png|jpeg`, `--quality 1..100` (default
70) and `--max-width PX`. A page mirror defaults to JPEG: pages are the
high-frame-rate case and JPEG frames are several times smaller. A window
or display mirror defaults to PNG, because desktop before 0.57.0 and Rune
before 0.1.0-alpha.17 cannot decode JPEG (they show the frame as invalid);
mobile decodes either. Pass `--format png` to a page mirror for an older
viewer. A page
scales its screencast in the browser and maps clicks by each frame's size;
the window mirror re-encodes its capture with macOS's `sips`, and on Linux
asks `grim` for the format and scale.

On Linux both mirrors run with no grant. `phux agent tree mirror APP` reads
AT-SPI over D-Bus: APP is the application's AT-SPI name (or `--pid` its
process), the accessibility bus comes from the session bus, and the document
has the macOS shape (`role`, `title`, `description`, `value`, `children`)
with AT-SPI role names such as `push button`. `phux agent surface mirror`
captures with `grim` on a wlroots compositor with screencopy (Hyprland,
Sway): `display` is every output, `display:OUTPUT` one, and a window `App`
or `App:title` is a Hyprland client's rectangle on a shown workspace, so
window targets need Hyprland. A pane whose server started outside the
graphical session finds `WAYLAND_DISPLAY` and `HYPRLAND_INSTANCE_SIGNATURE`
under `$XDG_RUNTIME_DIR`. A compositor that withholds frames (display
asleep, session locked) fails the capture after 10 s, before any surface is
opened.

Input to a Linux surface needs no extra tools on a compositor with the
wlroots virtual-pointer and virtual-keyboard protocols (Hyprland, Sway): the
mirror speaks them itself, typing through a keymap made for the text. A
click or scroll also needs Hyprland, whose `hyprctl` places the frame in the
layout. Without those protocols it falls back to `wtype` for keys and text,
and `wlrctl` or `ydotool` (with `ydotoold`) for the pointer. While input
arrives the host shows who is in control: a desktop notification on Linux,
closed 10 s after the last input; a Notification Center banner on macOS.
The server logs each intent at info with the sender's credential id (or
`owner`), the resource, its kind, the length of any text (never the text)
and its outcome, one line per burst plus a summary.
