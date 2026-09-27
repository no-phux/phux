---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-09-09
---

# Superlogical terminal demo reconstruction

**TL;DR.** A source-linked reconstruction of a 6:16 Superlogical remote-host
demo: feature inventory, interaction design and the phux gap map that turned
it into beads. The central idea is one persistent terminal experience across
local and remote machines. This is research from a demo, not an official
product specification or phux commitment.

| Document | Contents |
|---|---|
| [Product reconstruction](SPEC.md) | Features, evidence classes, menu-only capabilities, scope and acceptance scenarios |
| [phux gap map](GAP.md) | Feature-by-feature status in phux and the beads it produced |
| [UX and design](UX.md) | Information architecture, flows, states, shortcuts and component anatomy |

## What the demo establishes

Persistent sessions outlive the desktop app. Local and remote sessions share a
host-grouped switcher, tabs, panes and directory navigation. Rex exposes remote
identity and session control through a CLI. Creating and killing a session from
the CLI updates the graphical session list. The directory picker browses the
active host and opens a new terminal at the selected path.

## Evidence rules

- **O — observed:** a result or interaction is visible in the supplied footage.
- **N — narrated:** the presenter states it; the video is not independent proof.
- **M — menu/help only:** a label exists; behavior was not exercised.
- **I — inferred/proposed:** reconstruction needed to make a coherent design.
- **U — unknown:** neither the recording nor narration settles it.

Claims cite frame IDs (`Sxx`) and/or narration ranges (`mm:ss`). The transcript
was machine-generated (mlx-whisper, whisper-large-v3-turbo) and is not
human-verified. The presenter's inset and keystroke overlay are recording
context, not terminal features.

## Source and limits

The third-party MP4 (376 s, H.264 540x360) is not committed. Original URL:
`https://video.twimg.com/amplify_video/2097423824325337088/vid/avc1/540x360/72u8WfUgvZtB-5FO.mp4`,
SHA-256 `4c10f8910b0b08b02338c9d36263788994c8c90fe44c0510e57abc89163c0c28`.
Re-extract a cited frame with `ffmpeg -ss <seconds> -i source.mp4 -frames:v 1 out.png`.
The screenshots, mockups, raw transcript and HTML viewer that accompanied the
original study were removed from the tree; they remain in git history.

At 540x360, tiny icon glyphs, several help flags, typography and some labels
remain ambiguous. Timing descriptions such as "fast" are qualitative; this
recording is not a latency benchmark. The transport, recovery algorithm and
login implementation were left undisclosed by the presenter.
