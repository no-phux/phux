---
name: self-heal
description: Investigate an error in the phux system and fix it by PR in an owned repository.
---
Where errors show up (read them from a shell pane):
- phux server: `phux logs --server -n 400` (canonical log `~/.local/state/phux/server.log`, rotated `.1`..`.4`); `phux doctor`; `phux status --json`.
- phux clients: `phux logs` lists per-pid client logs; `phux logs --client --pid N`.
- Crashes: `~/Library/Logs/DiagnosticReports/phux*.ips` (server, CLI, desktop).
- Bug bundles: `phux report` lists them, `phux report show ID` prints one.
- Mobile: diagnostics the phone forwards arrive as `[error intake]` items with source `mobile`; the mobile app's own repository is phux-mobile.
- CI: `gh run list --repo OWNER/NAME --branch main --status failure`, then `gh run view ID --log-failed`.
- Your own failures: a tool that keeps failing (wrong selector, phux CLI refusing) is a finding too; check `phux runtime-info` and the CLI's error before retrying.

Loop:
1. Reproduce or pin the failing line; find the code that emits it (`rg` in the checkout).
2. Decide: real defect, environment, or noise. Noise gets one line in your report and nothing else.
3. For a defect in an owned repository: `git -C <repo> fetch origin`, then `git -C <repo> worktree add ../<repo>-fix-<topic> -b fix/<topic> origin/main` and work only there. Read its AGENTS.md first; run the narrowest check that covers the fix, then the repository's PR gate.
4. Commit with a conventional message, push the branch, `gh pr create --fill` (draft if unsure). Never push to main, merge, or release.
5. Never touch the installed phux or its running server: no `phux upgrade`, no `phux kill` of the server, no copying builds over installed binaries, no reuse of the production socket for test servers.
6. Report: the error, the cause, the PR link, what still needs a human.
