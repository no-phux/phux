# Product Reference Capture

Use public product demos as behavioral evidence while adapting the interaction
to Phux Cockpit's native model and design system. Raw media and informal notes
stay local; implementation and durable product decisions remain tracked.

## Capture An X Post

```sh
./scripts/capture-reference.sh 'https://x.com/.../status/...' short-label
```

The script creates `.references/short-label/` containing:

- `source.url` and `metadata.json` for provenance
- `media/` with the post video, description, thumbnail, and subtitles when present
- `frames/` with at most roughly 60 review frames from the first downloaded video
- `notes.md` for local observations and the proposed Cockpit adaptation

`.references/` is gitignored. Send an X URL to the coding agent and it can run
the capture, inspect the video and frames, trace the relevant Cockpit code, and
implement the adapted behavior. A label is optional and defaults to the source
extractor plus post ID. Repeating a capture with the same label and URL resumes
an interrupted download without replacing local notes.

Public posts normally work without login. If X requires authentication, reuse a
local browser session:

```sh
REFERENCE_COOKIES_FROM_BROWSER=safari \
  ./scripts/capture-reference.sh 'https://x.com/.../status/...'
```

The script uses an installed `yt-dlp`, or runs it through `uvx` when available.
It requires `jq`; frame extraction additionally uses `ffmpeg` and `ffprobe`.

Do not ship copied media, branding, or source assets. Reproduce the useful
behavior in Cockpit's own product language, and verify it against
`docs/PRODUCT_DIRECTION.md`, `docs/DESIGN_SYSTEM.md`, and the real macOS app.
