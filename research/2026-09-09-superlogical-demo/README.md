---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-09-09
---

# Superlogical terminal demo reconstruction

**TL;DR.** A source-linked reconstruction of a 6:16 Superlogical remote-host
demo: full generated transcript, 32 selected screenshots, feature inventory,
interaction design, visual guidance, and interactive mockups. The central idea
is one persistent terminal experience across local and remote machines. This
is research from a demo, not an official product specification or phux commitment.

## Start here

Open **[index.html](index.html)** for the video, caption download, screenshot
gallery and clickable mockup. It is self-contained and works from disk; serving
over HTTP with byte-range support enables captions and reliable video seeking:

```sh
cd research/2026-09-09-superlogical-demo
uv run --with rangehttpserver python -m RangeHTTPServer 8766 --bind 127.0.0.1
```

Then visit `http://localhost:8766`. Mockup actions simulate the demonstrated
flows; they do not connect to hosts or execute commands.

| Deliverable | Contents |
|---|---|
| [Transcript](TRANSCRIPT.md) | Complete timestamped narration, provenance and normalization notes |
| [Plain text](transcript.txt) / [WebVTT](transcript.vtt) | Portable transcript and player captions |
| [Product reconstruction](SPEC.md) | Features, evidence classes, menu-only capabilities, scope and acceptance scenarios |
| [phux gap map](GAP.md) | Feature-by-feature status in phux and the beads it produced |
| [UX and design](UX.md) | Information architecture, flows, states, shortcuts, component anatomy and mockup decisions |
| [Screenshot manifest](frames.json) | 32 stable evidence IDs, seek times and captions |
| [Contact sheet 1](screenshots/contact-01.jpg) | Local persistence, remote connection and system login |
| [Contact sheet 2](screenshots/contact-02.jpg) | Session navigation, rename, CLI and identity |
| [Contact sheet 3](screenshots/contact-03.jpg) | Empty state, tabs, relaunch and session kill |
| [Contact sheet 4](screenshots/contact-04.jpg) | Local and remote directory pickers |
| [Mockup contact sheet](mockups/contact-sheet.jpg) | Eight exported reconstruction scenes, labeled as mockups |
| [Source metadata](source.json) | URL, SHA-256, codec data, duration and transcription method |
| Original video / [raw ASR](transcript-raw.json) | Preserved evidence; the video is not committed (see below) |

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

Claims in the spec include evidence IDs and/or narration ranges. Screenshots
preserve source pixels, including the presenter's inset and keystroke overlay;
those overlays are not assumed to be terminal features. Mockups intentionally
omit recording overlays and use legible replacement text.

## Coverage and limits

The source has 376.363 seconds of media, 376.150 seconds of video, H.264 at
540×360, and stereo AAC audio. Its metadata records creation at
2026-09-08T20:40:47Z; that is not independently verified publication time.
The direct MP4 identifies neither the tweet nor the author account. Product
naming follows the generated narration; no external release claims are added.

Review used 188 two-second overview samples, finer sampling of brief rename
and CLI transitions, and 32 individually extracted/visually checked frames.
This covers the distinct demonstrated product states found in the clip;
it does not establish the terminal's entire shipping feature set. Brief,
unreviewed frame-level transitions may exist between samples. Selected-frame
times are requested FFmpeg seek times, resolved to the next available frame.

At 540×360, tiny icon glyphs, several help flags, typography and some labels
remain ambiguous. The spec avoids treating enlargement as new detail. Timing
descriptions such as “fast” are qualitative: this recording is not a latency
benchmark. The transport, recovery algorithm and login implementation are
explicitly left undisclosed by the presenter.

## Reproducing the evidence

The third-party MP4 is deliberately kept out of this public repository.
`source.json` records its original URL and SHA-256; place a verified copy at
`source.mp4` in this directory (it is git-ignored) to enable the viewer's video
player and the commands below.

Verify the MP4 against the SHA-256 in `source.json`. Re-extract any screenshot
using its time in `frames.json`, for example:

```sh
ffmpeg -ss 295.5 -i source.mp4 -frames:v 1 local-directory.png
```

The transcript was generated with:

```sh
uv run --with mlx-whisper python -c 'import mlx_whisper,json; r=mlx_whisper.transcribe("source.mp4",path_or_hf_repo="mlx-community/whisper-large-v3-turbo",language="en",verbose=False); open("transcript-raw.json","w").write(json.dumps(r,indent=2))'
```

Recognition can vary with model and library revisions. Original raw output is
retained so the delivered transcript does not depend on rerunning inference.

## Validation record

Validated on 2026-09-09:

- `bash scripts/doctor.sh docs`: zero prerequisite problems.
- `bash scripts/check-docs.sh`: no documentation violations.
- `node --check research/2026-09-09-superlogical-demo/viewer.js`: syntax passes.
- Artifact audit: matching source SHA-256, 32 unique 540×360 frames with valid
  times, 105 ordered transcript segments, corresponding WebVTT/plain-text
  entries, Markdown metadata and 53 local artifact links.
- Headless Chrome via Playwright: media load and timestamp seeking, transcript
  and gallery population, gallery filter, all eight scenes, directory browsing,
  new tabs, original split restoration, rename, simulated host connection,
  empty-session identity, Escape dismissal and a 390px-wide layout. No
  JavaScript page errors were observed. Eight desktop mockup PNGs were exported.
- Parent visual inspection of source contact sheets and reconstructed exports.
  A fresh-context reviewer found one incorrect CLI-help transcription (`local`
  instead of `serve`); the inventory was corrected. No other material findings
  were reported in that evidence review.

The HTTP test exposed that a plain Python static server did not provide reliable
video seeking. The documented range-capable server passed the same seek check.
These are scoped artifact/viewer checks, not product/runtime or full CI results.
