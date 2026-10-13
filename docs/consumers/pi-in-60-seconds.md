---
audience: humans
stability: evolving
last-reviewed: 2026-10-13
---

# pi + phux in 60 seconds

**TL;DR.** Two panes in one phux session: your shell on the left, pi on the
right. pi drives the left pane. You can take the keyboard back at any point.

## Install

On Apple-silicon macOS or a supported Linuxbrew host, `brew trust` applies
to Homebrew 6 and newer:

```sh
brew trust --tap no-phux/tap # Homebrew 6+ only
brew tap no-phux/tap
brew install no-phux/tap/phux
pi install npm:@phux/pi
```

`@phux/pi` needs Node.js 22 or newer, pi, and `phux` on `PATH`. Other phux
install methods are in the [installation guide](../INSTALL.md).

## 1. Split a session (0–10s)

```sh
phux new work
```

That creates a session named `work` and attaches to it. The prefix is
`Ctrl-A`: press Ctrl and A together, release both, then press `%`. The
shell stays on the left. Focus moves to the new pane on the right. Run
`pi` there.

## 2. Point pi at the shell (10–20s)

In pi, enter `/phux` and pick the left shell pane. Don't pick pi's own pane.
The status line shows the target.

## 3. Let pi work (20–35s)

Ask pi: "snapshot the left pane, then run the tests there." `phux_snapshot`
reads the pane, and `phux_run` types the command into that live shell. The
exit code comes back to pi.

## 4. Take the keyboard (35–50s)

If a test fails, click the left pane and fix it by hand in the same shell.
Then ask pi to snapshot again so it sees your change.

## 5. Rerun (50–62s)

pi reruns the tests in the same pane.

## 6. Detach and come back

Press `Ctrl-A`, release both keys, then `d`. From the shell, run:

```sh
phux attach
```

With no session name, that attaches to the most recently focused session.
Both panes are still there.

## Next

- [pi integration reference](./pi.md)
- [Recording the product demo](../demo.md)
