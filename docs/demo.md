---
audience: contributors
stability: stable
last-reviewed: 2026-06-10
---

# Recording the README demo

**TL;DR.** The README demo is the first moving proof on the page: two short
beats — *modern terminal content survives a detach/reattach*, then *the same
thing driven headless* — recorded as a GIF at `docs/assets/demo.gif`. Use a
real screen-recorder for future full-fidelity takes of the first beat
(asciinema players don't render kitty graphics, which is the whole point);
asciinema is fine for the second.

---

## The two beats

Keep it under ~15 seconds. The pitch is "look how little is going on, and yet."

Run [`scripts/demo-setup.sh`](../scripts/demo-setup.sh) first: it creates the
`demo` session headlessly and prints this runbook with the resolved binary
path.

**Beat 1 — it survives (≈8s).** In a graphics-capable terminal (Ghostty,
kitty, WezTerm), attach, paint something a lesser multiplexer would mangle,
detach, reattach. The viewer should see the fancy content come back *intact*.

```sh
phux attach demo
# in the pane: truecolor gradient, curly underlines, OSC 8, an inline image
bash docs/assets/payload.sh
# detach:
Ctrl-A d
# reattach — the gradient and the image are still there, pixel-for-pixel:
phux attach demo
```

**Beat 2 — an agent could've done that (≈6s).** Drop to a second terminal and
drive the *same* session without a TTY. This is the line that makes a skimmer
stop.

```sh
phux run demo "cargo test --quiet"   # runs in the live pane, prints the exit code
phux watch --json demo | jq .        # live events scrolling by; Ctrl-C to cut
```

Cut on the JSON events scrolling. No narration, no captions. Let the
juxtaposition do it.

## The payload

[`docs/assets/payload.sh`](./assets/payload.sh) is the recorded payload, so
every take paints the same pixels. It puts four things on screen, each one a
distinct claim:

- a **truecolor sweep** sized to the terminal width — every column a distinct
  24-bit color; the thing a palette-quantizing multiplexer carries badly;
- **curly underline** (SGR 4:3) with a truecolor underline color (SGR 58);
- an **OSC 8 hyperlink**;
- a **kitty-graphics image** — the thing tmux literally cannot carry across a
  reattach. The PNG is embedded in the script as base64 and emitted straight
  through the graphics protocol, so the payload needs no `kitty` binary — only
  a terminal that renders the protocol.

Run it from inside the attached pane (not headlessly before attaching), so the
content is painted at the size you are recording at.

## Tools

- **GIF (Beat 1, required):** any screen-recorder that captures real pixels —
  [`vhs`](https://github.com/charmbracelet/vhs) if you want it scripted and
  deterministic, or a plain screen-capture-to-GIF. Kitty-graphics frames must
  survive into the GIF, so this can't be a terminal-only recorder. This has not
  changed: phux's own recorder replays cells, and an inline image is not a
  cell, so Beat 1's payload cannot survive a cast-and-re-render round trip.
- **Beat 2:** phux records itself. `phux rec demo -o beat2.cast --duration 20`
  captures the pane as a pure observer, and `phux rec --from beat2.cast -o
  beat2.gif` renders the GIF with no `asciinema` and no `agg` in the loop. See
  [`consumers/recording.md`](./consumers/recording.md) for the flags; the
  `--fps` and `--idle-limit` knobs are the size levers.
- Target **≤ 2 MB** so it loads before the reader scrolls past it. Trim
  generously; the demo is a hook, not a tutorial.

## Wiring it in

The README already points at `docs/assets/demo.gif`. When replacing the asset
with a higher-fidelity take, keep the same path and re-run `bash
scripts/check-docs.sh` plus a real attach smoke.
