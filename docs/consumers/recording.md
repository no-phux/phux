---
audience: humans, consumers
stability: evolving
last-reviewed: 2026-09-12
---

# Recording a session

**TL;DR.** Record one pane with `phux rec` or an attached session with
`phux --rec`. Export asciinema casts, GIFs, or APNGs without external tools;
play a cast back in a new pane with `phux play`. Headless recording observes
without resizing; interactive recording captures the client's composited
output. Neither records input events.

![A phux recording, recorded with phux](../assets/recording-demo.gif)

---

## 1. The two surfaces

`phux rec [TARGET] -o PATH` records one pane without attaching the session
or resizing the pane. Use it for scripted capture or to record a pane
someone else is using. It connects to a local server over the UDS
(`--socket` overrides the path), not QUIC or WebSocket.

```sh
phux rec -o demo.gif                       # the focused pane, until Ctrl-C
phux rec work:1.0 -o demo.cast --duration 30
phux rec @7 -o demo.png --fps 20           # .png means APNG
```

`phux --rec PATH` records the attached client's composited output: tiled
panes, dividers, status bar, sidebar, overlays, and cursor. The flag applies
only to `phux` and `phux attach`; other verbs reject it and point to `phux rec`.

```sh
phux --rec demo.gif                        # attach and record the whole session
phux attach work --rec demo.cast
phux --rec demo.out --rec-format gif       # explicit format wins; path as typed
```

Ctrl-C ends a headless capture successfully, writing and exporting what it
recorded. Interactive capture finishes on detach and prints its result after
leaving the alternate screen.

## 2. What a recording is not

- **Timing is the server's paint cadence, not your keystrokes.** The server
  coalesces PTY bytes at its output pacing rate (default 60 Hz) before it
  emits them, and the wire carries no per-byte timestamp. Sub-frame cadence is
  therefore not recoverable: a fast flood replays chunkier than it looked
  live. The interactive surface has the same limit one layer up, because the
  client coalesces bursts of frames into one paint.
- **A recording opens on the viewport, not the history above it.** The
  observer's priming snapshot is requested without scrollback, so a recording
  started mid-session begins with the screen as it stands, not with what
  scrolled past before you started.
- **Glyph coverage is narrow.** The animation is drawn with a 1-bit bitmap
  face: Latin, Greek, Cyrillic, Braille, box drawing, and Powerline. CJK and
  other wide glyphs and color emoji render as tofu boxes. This is a permanent
  encoder limit that keeps GIF quantization lossless
  ([ADR-0060](../adr/0060-self-contained-session-recording.md)).
  It affects rendered animations, not `.cast` files.

Input events are never recorded on either surface, and there is no opt-in
flag. Kitty-graphics images do not survive re-rendering: the replayer draws
cells, not images.

**Agent-session streams are not recorded.** A cast is a terminal artifact;
an agent session's JSON record stream is read with `phux agent log`
(ADR-0103).

## 3. Formats

The output extension picks the format, case-insensitively:

| Extension | Format | Notes |
|---|---|---|
| `.cast` | asciinema cast | The archival artifact. Text, diffable, small. |
| `.gif` | animated GIF | Shareable and embeddable with no player. |
| `.png`, `.apng` | animated PNG | Truecolor, no palette, 1 ms timing. |
| *(none)* | animated GIF | `.gif` is appended to the path you gave. |

`--format` (or interactive `--rec-format`) overrides the extension without
changing the path. Unknown extensions are rejected; phux will not write a
GIF named `demo.mp4` unless you explicitly select that format.

Every export uses a cast as its source. For GIF or APNG output, phux creates
a temporary cast and deletes it after a successful render. If rendering
fails, it keeps the cast and prints its path so the capture can be recovered.
To keep a reusable source, record a cast and render it later:

```sh
phux rec --from demo.cast -o demo.gif --fps 20 --idle-limit 1.5
phux rec --from demo.cast -o smaller.gif --max-bytes 2000000
```

`--from` runs offline without contacting the server. It can also transcode:
`--from v2.cast -o v3.cast --cast-version 3`.

### asciicast version

The default is **v2**, which every asciinema reader plays; v3 is not
backward compatible (a v2-only reader plays it at the wrong speed). Pass
`--cast-version 3` when your reader supports it.

## 4. Tuning the capture and the render

| Flag | Default | What it does |
|---|---|---|
| `--fps N` | `10` | Sample rate, snapped to the nearest of 5, 10, 20, 25, 50 (periods that divide 1000 ms exactly), so `--fps 30` records at 25. |
| `--idle-limit SECS` | `2.0` | Collapse any pause longer than `SECS` down to `SECS`. `0` disables. |
| `--max-bytes N` | `8388608` | Stop encoding at this size, close the container cleanly, and report the artifact as truncated. |
| `--duration SECS` | *(none)* | Stop the capture after `SECS`. Without it, recording runs until Ctrl-C or the pane exits. |

The idle clamp applies once, before both the cast write and the render, so
they agree. `--fps` is the main size lever; idle stretches cost no frames.

Interactive `--rec` uses the defaults. Record a `.cast` and re-render with
`--from` to change them.

## 5. Output

On success, one line on stdout:

```
phux: wrote demo.gif (184.3 KiB, 211 frames, 42.1s)
```

`--json` prints only the result object on stdout; its shape is documented in
[`agents.md`](./agents.md). Headless progress (`recording... 12s (340 events)`)
goes to stderr unless `--json` is set. Interactive capture prints nothing
while it owns the alternate screen.

## 6. Playing a recording back

`phux play FILE.cast [TARGET]` creates a pane whose PTY reads from the
recording and prints its Terminal id. To play directly in your current
terminal instead, use `asciinema play FILE.cast`; it needs no phux server.

```sh
phux play demo.cast                        # a pane beside the focused one
phux play demo.cast work:1.0 --speed 2     # twice as fast, beside that pane
phux play demo.cast --loop --idle-limit 0.5
phux play demo.cast --json                 # {"schema_version": 1, "terminal_id": 7, ...}
```

The result is an ordinary pane: attach, snapshot, resize, watch, re-record,
or kill it like any other.

TARGET chooses placement, not a pane to overwrite. Playback creates a new
pane beside TARGET, splitting its window as `phux spawn --target` does.
The default is `.` (the focused pane). Playback cannot replace an existing
pane's process.

| Flag | Default | What it does |
|---|---|---|
| `--speed N` | `1` | Divides wall-clock time: `2` is twice as fast, `0.5` half. Between 0.01 and 100. No events are dropped, merged, or resampled at any speed. |
| `--idle-limit SECS` | *(the recording's own)* | Collapse pauses longer than `SECS`. Defaults to the `idle_time_limit` the cast declares, so playback agrees with the recorder that wrote it; `0` plays the raw timeline. |
| `--loop [N]` | *(one pass)* | Bare `--loop` repeats until the pane is killed; `--loop N` plays it N times. Between passes the screen is soft-reset and cleared, never fully reset — a full reset would drop the grid size and make pass two wrap differently from pass one. |
| `--no-fit` | *(off)* | Leave the pane's grid alone. See below. |
| `--close` | *(off)* | Close the pane when playback ends instead of holding the final frame. |
| `--split`, `--ratio` | `horizontal`, `0.5` | Placement of the new pane, as on `phux spawn`. |

Before playback, the pane is resized to the cast header's grid; recorded
`r` events resize it again. VT bytes depend on geometry: a narrower grid
changes line wrapping and cursor placement, which can make playback
unreadable. If an attached client's size policy overrides a resize, playback
warns and continues. Every `defaults.window-size` policy except `manual`
lets a client's viewport control pane size; see [Layout](./tui.md#layout).
`--no-fit` suppresses both the initial fit and recorded resizes.

When playback ends, the pane holds its final frame until killed, allowing
`phux snapshot` without a timing race. `--close` ends it instead. Only `o`
and `r` events drive the pane; recorded input, markers, and exit status are
ignored. Pause, seek, and scrubbing are not supported.

## 7. Where this fits

Neither recording nor playback adds anything to the wire: `phux rec` rides the
`ATTACH_RESOURCE` observer subscription ([`../spec/L1.md`](../spec/L1.md)
§5.1) with in-process encoders, and a playback pane is the phux binary
re-invoked as the spawned command. Rationale:
[ADR-0060](../adr/0060-self-contained-session-recording.md) and
[ADR-0064](../adr/0064-playback-as-a-pane.md).
