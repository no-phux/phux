---
title: "Programs tell phux what they’re doing"
summary: "Watch a real OSC 7501 program move from working to waiting for approval to done—with live badges and the same state in the API."
publishedAt: "2026-10-08"
duration: 29.5
video: "/demos/program-status-osc7501/video.mp4"
poster: "/demos/program-status-osc7501/poster.webp"
captions: "/demos/program-status-osc7501/captions.vtt"
version: "0.53.0"
---

This is a real recording of phux 0.53.0 on macOS. A small Python program checks
three sample inputs and emits OSC 7501 reports through its normal terminal
output. The approval pause is a real wait for Enter—not a simulated UI.

## What to watch

- **0:04 — Working.** The program reports its state and progress while computing
  checksums for sample inputs. The tab and Agents list show that it is busy.
- **0:10 — Needs you.** A `blocked` report with `kind=permission` changes the
  badges while the program waits for approval.
- **0:16 — Done.** After Enter, the program finishes and returns to Bash. Its
  completed result stays visible through the next shell prompt.
- **0:22 — Inspect it.** `show_status` prints the active record returned by
  `phux resource show --json @1`. The same record is available to MCP clients
  and metadata subscribers.

## Try reporting a state

Any program can write the protocol over its existing PTY. No phux-specific
SDK or local socket call is needed:

```sh
printf '\033]7501;state=working:app=my-tool:progress=35\033\\'
printf '\033]7501;state=blocked:kind=permission:app=my-tool\033\\'
printf '\033]7501;state=done:app=my-tool\033\\'
```

Read the [program-status contract](https://docs.phux.sh/wire/l3#373-phuxprogram-statusv1--osc-7501-program-status)
for record lifetime, hierarchical jobs, messages, and support detection, or the
[full OSC 7501 specification](https://www.superlogical.com/rex/docs/build/program-status)
for an integration in your own tool.
