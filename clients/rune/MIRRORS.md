# Read-only mirrors

The palette lists live AccessibilityTree and PixelSurface resources alongside
terminals. Open one directly, switch its tab, or focus it beside a terminal in
the session's arrangement. Trees use a selectable native JSON inspector; PNG
surfaces use Rune's image decoder and fit without cropping. These are mirrors,
not embedded browsers: paste, file drops, and terminal resizing never reach a
document pane. A surface whose producer declared a control class (a page or
surface mirror) reads **Live mirror**: a click, a drag, the wheel, plain keys
reach its producer as document input. Any other document is a **Read-only mirror**,
and a refused send turns a live one **View only**. Closing its view does not
stop the producer.

Markdown documents use bundled tmd 0.2.0 in print mode inside the native
selectable, scrollable text pane. **Source** shows the original Markdown,
and **Rendered** returns to the renderer. This first adapter supplies text
layout and tables, not tmd's interactive terminal UI, colours, clickable links
or images. The renderer is replaceable with a trusted argv; `[]` selects
source only ([config](../../docs/config.md#markdown-viewer)). Missing, failed,
oversized or slow renderers explain why and leave source visible. The iOS
default remains source. Remote documents render locally without fetching
their links or assets.

To start a source, choose **Start page/tree/surface mirror…** in the palette,
name the existing source, then confirm. Rune starts a dedicated producer
terminal using the selected local CLI and socket, never the focused shell.
Page capture needs Chrome debugging on localhost:9222 with a separate profile.
Tree and surface capture require macOS Accessibility or Screen Recording
permission respectively. The confirmation explains the prerequisite; producer
exit produces an actionable notice. Ctrl-C in its terminal stops it.

Starting a producer remotely is refused rather than guessing that host's
platform, CLI, or permissions. Run the producer on the source host and reopen
its published resource here. Opening or moving its view is not process or
application migration.

## Viewer seam

The Markdown viewer seam is an argv filter: bounded UTF-8 source in,
plain UTF-8 text out at the pane's column width. The bundled tmd adapter
and the source adapter share native selection, scrolling and layout; a
trusted configured argv can replace tmd without changing the document
stream, producer or pane. Publication-local presentation state rejects
obsolete results, while window-wide worker admission survives connection
replacement and resource retirement. Renderer failures retain source.
This is desktop presentation policy, not a new resource kind or wire codec.
