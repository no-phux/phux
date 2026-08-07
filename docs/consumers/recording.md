---
audience: humans, consumers
stability: evolving
last-reviewed: 2026-07-27
---

# Recording a session

**TL;DR.** How to record a phux pane or a whole attached session and export it
as an asciinema cast, an animated GIF, or an APNG, and how to play a cast back
as a live pane. Two capture surfaces share one set of rules: a headless
observer that never touches the pane it watches, and a tee on the interactive
client's own composited output. No external tools are involved.

![A phux recording, recorded with phux](../assets/recording-demo.gif)

---

## 1. The two surfaces

**`phux rec [TARGET] -o PATH`** records one pane, headlessly, from outside.
It subscribes as a pure observer: it does not attach the session and never
resizes the pane, so it is safe to run against a session a human is using
right now. This is the one to reach for when you want a clean artifact of a
single pane, when you want to script a recording, or when you are not the
person sitting in the session. It talks to a local server over the UDS
(`--socket` overrides the path); it does not dial QUIC or WebSocket.

```sh
phux rec -o demo.gif                       # the focused pane, until Ctrl-C
phux rec work:1.0 -o demo.cast --duration 30
phux rec @7 -o demo.png --fps 20           # .png means APNG
```

**`phux --rec PATH`** records the session you are attached to, as you use it.
It tees the client's own composited output stream, so the recording contains
exactly what your glass received: panes tiled per the layout, dividers, the
status bar, the sidebar, overlays, and the cursor. The flag applies to `phux`
and `phux attach` only; on any other verb it is an error that points you at
`phux rec`.

```sh
phux --rec demo.gif                        # attach and record the whole session
phux attach work --rec demo.cast
phux --rec demo.out --rec-format gif       # explicit format wins; path as typed
```

Ctrl-C during a headless capture is a **success**: you asked the recording to
stop, and what was captured up to that point is written and exported. The
interactive surface finishes when the session detaches, and prints its
one-liner only after the alt screen is down.

## 2. What a recording is not

Worth knowing before you record something long, rather than after.

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
  other wide glyphs and color emoji render as tofu boxes. This is a deliberate
  and permanent consequence of the encoder design
  ([ADR-0060](../../ADR/0060-self-contained-session-recording.md)) — it is
  what keeps GIF quantization lossless — not a gap waiting to be filled. A
  `.cast` has no such limit; only the rendered animation does.

Two more worth knowing: **input is never recorded** (no keystroke events, on
either surface, with no opt-in flag — passwords do not belong in a recording),
and kitty-graphics images do not survive a re-render, because the replayer
draws cells and an image is not one.

## 3. Formats

The output extension picks the format, case-insensitively:

| Extension | Format | Notes |
|---|---|---|
| `.cast` | asciinema cast | The archival artifact. Text, diffable, small. |
| `.gif` | animated GIF | Shareable and embeddable with no player. |
| `.png`, `.apng` | animated PNG | Truecolor, no palette, 1 ms timing. |
| *(none)* | animated GIF | `.gif` is appended to the path you gave. |

`--format` (or `--rec-format` on the interactive surface) overrides the
extension outright and leaves the path exactly as you typed it. An extension
phux does not recognize is an error naming the three it does — it will not
silently write a `demo.mp4` that is not an MP4.

**The `.cast` is the source of truth and the animation is a derivative.** Every
export goes through a cast, even when you asked for a GIF; for a GIF or an
APNG it is an intermediate in the temp directory, deleted once the artifact
lands. If the render fails, that intermediate is deliberately *kept* and its
path printed, so a forty-minute capture is never lost to an encoder bug. Keep
the cast when the recording matters and re-render it later:

```sh
phux rec --from demo.cast -o demo.gif --fps 20 --idle-limit 1.5
phux rec --from demo.cast -o smaller.gif --max-bytes 2000000
```

`--from` never touches the server. It is a pure offline re-render, and it also
transcodes: `--from v2.cast -o v3.cast --cast-version 3`.

### asciicast version

The default is **v2**. asciicast v3 is not backward compatible with v2 — the
header schema changed and event times became relative intervals — so a v2-only
reader that tolerates a v3 header plays a four-minute recording in a fraction
of a second. v2 is read by asciinema CLI 2.x and 3.x, player 2.6 and later,
and server builds back to 2017; v3 needs CLI 3.0, player 3.10.0, and a 2025
server. There is no consumer that reads v3 but not v2. Pass
`--cast-version 3` when you know your reader is new enough and you want it.

## 4. Tuning the capture and the render

| Flag | Default | What it does |
|---|---|---|
| `--fps N` | `10` | Sample rate, snapped to the nearest of 5, 10, 20, 25, 50. Those are the rates whose period divides 1000 ms exactly, which is what keeps timing drift-free and GIF delays exact in centiseconds. `--fps 30` therefore records at 25. |
| `--idle-limit SECS` | `2.0` | Collapse any pause longer than `SECS` down to `SECS`. `0` disables. |
| `--max-bytes N` | `8388608` | Stop encoding at this size, close the container cleanly, and report the artifact as truncated. |
| `--duration SECS` | *(none)* | Stop the capture after `SECS`. Without it, recording runs until Ctrl-C or the pane exits. |

The idle clamp is applied once, to the shared event list, before both the cast
write and the render — so the `.cast` and the GIF derived from it can never
disagree about how long a pause was.

`--fps` is the main size lever. An idle terminal costs no frames at all
(a sampled frame with nothing dirty extends the previous frame's delay instead
of emitting a new one), so the cost of a higher rate is paid only by the parts
of the recording that are actually moving.

The interactive `--rec` surface has none of these knobs: it renders with the
defaults. If you want a different rate, keep the `.cast` and re-render with
`--from`.

## 5. Output

On success, one line on stdout:

```
phux: wrote demo.gif (184.3 KiB, 211 frames, 42.1s)
```

With `--json`, one object on stdout and nothing else. The shape is documented
in [`agents.md`](./agents.md) §4.14. Progress (`recording... 12s (340
events)`) goes to stderr and only on the headless surface, and is suppressed
under `--json`; the interactive surface says nothing at all while the session
is up, because it owns the alt screen.

## 6. Playing a recording back

`phux play FILE.cast [TARGET]` creates a **pane whose PTY is fed from the
recording**, and prints its Terminal id. It does not paint the cast onto the
terminal you are typing in — for that, `asciinema play FILE.cast` is the right
tool and needs no phux server at all.

```sh
phux play demo.cast                        # a pane beside the focused one
phux play demo.cast work:1.0 --speed 2     # twice as fast, beside that pane
phux play demo.cast --loop --idle-limit 0.5
phux play demo.cast --json                 # {"schema_version": 1, "terminal_id": 7, ...}
```

The point is what the result *is*: an ordinary pane. Attach to it, read its
grid with `phux snapshot @7`, resize it with `phux resize @7 100x30`, watch it
from an agent over the same observer subscription `phux rec` uses, share it
with a second client, re-record it, or end it with `phux kill @7`. Everything
already aimed at panes works, because it is not a special object.

**TARGET says where the pane goes, never what gets overwritten.** The playback
pane is created *beside* TARGET, splitting its window exactly as
`phux spawn --target` does; TARGET itself is untouched. The default is `.`,
the focused pane, so a playback appears next to whatever you are looking at.
There is no flag that plays into a pane that already has a shell in it — a
pane's process is its identity, and the only way to give one to a recording
would be to kill the shell first, which is `phux kill` spelled confusingly.

| Flag | Default | What it does |
|---|---|---|
| `--speed N` | `1` | Divides wall-clock time: `2` is twice as fast, `0.5` half. Between 0.01 and 100. No events are dropped, merged, or resampled at any speed. |
| `--idle-limit SECS` | *(the recording's own)* | Collapse pauses longer than `SECS`. Defaults to the `idle_time_limit` the cast declares, so playback agrees with the recorder that wrote it; `0` plays the raw timeline. |
| `--loop [N]` | *(one pass)* | Bare `--loop` repeats until the pane is killed; `--loop N` plays it N times. Between passes the screen is soft-reset and cleared, never fully reset — a full reset would drop the grid size and make pass two wrap differently from pass one. |
| `--no-fit` | *(off)* | Leave the pane's grid alone. See below. |
| `--close` | *(off)* | Close the pane when playback ends instead of holding the final frame. |
| `--split`, `--ratio` | `horizontal`, `0.5` | Placement of the new pane, as on `phux spawn`. |

**The pane is fitted to the recording.** Before the first byte, the pane is
resized to the cast header's grid, and each `r` event in the recording is
applied the same way. A cast is bytes that were correct at one geometry:
played into a narrower pane, wrapped lines wrap in the wrong places and
absolute cursor addresses land somewhere else, and the result is not
approximate but unreadable. When the resize does not hold — an attached
client's viewport owns a pane's size under every `defaults.window-size` policy
but `manual`, see [`tui.md`](./tui.md) §4.2 — playback says so in one line and
plays anyway. `--no-fit` suppresses the header fit and the recorded resizes
alike.

**When the recording ends, the pane holds its final frame** until you kill it.
That is deliberate: the painted screen is the artifact, and a pane that erased
itself on the last byte would make `phux snapshot` a race. `--close` ends the
pane instead, and Ctrl-C in an attached playback pane stops it.

Only `o` (output) and `r` (resize) events drive a pane. Recorded input (`i`),
markers (`m`), and the recorded exit status (`x`) are read and ignored —
replaying input would type a recording's keystrokes into a live PTY, which is
not what anyone means by "play".

There is no pause, no seek, and no scrubbing. Adding them would make this the
shell-level player [ADR-0064](../../ADR/0064-playback-as-a-pane.md)
deliberately does not build.

## 7. Where this fits

Recording adds nothing to the wire. It rides the `ATTACH_TERMINAL` observer
subscription that [`../spec/L1.md`](../spec/L1.md) §5.1 already specifies —
snapshot, then deltas, no session attach, no resize — and the GIF and APNG
encoders are in-process, so `phux rec` works on a machine with no `agg`, no
`vhs`, and no `ffmpeg`. The reasoning, and the design spaces it closes, are in
[ADR-0060](../../ADR/0060-self-contained-session-recording.md).

Playback adds nothing to the wire either. `SPAWN_TERMINAL` already carries a
command, the server already tells a spawned pane its own id and socket, and
`TERMINAL_RESIZE` already exists — so the pane's "process" is simply the phux
binary re-invoked in a mode that writes a cast to its own stdout.
[ADR-0064](../../ADR/0064-playback-as-a-pane.md) has the reasoning, including
why the shell-level player stays unbuilt.
