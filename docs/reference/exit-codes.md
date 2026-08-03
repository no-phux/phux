---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux exit codes reference

**TL;DR.** The canonical exit-code table: 0 success, 1 failure, 2 usage error or server refusal, 3 partial-fleet unanswerable, 124 `wait` timeout, 125 `run` timeout. Rendered from the same in-code table `phux --help`'s EXIT STATUS section uses, so the two cannot disagree.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Every exit code the `phux` binary uses, from the canonical in-code table that also renders the EXIT STATUS section of `phux --help`. The codes are chosen so a script can branch: `3` is distinct from `1` because retry is right for `3` and wrong for `1`; `run`'s timeout is `125` rather than `124` because `run` mirrors the exit code of the command it ran, and the child itself may legitimately exit `124`.

| Code | Meaning |
|---|---|
| `0` | Success. |
| `1` | Failure: no server, no such target, or the verb itself failed. |
| `2` | Usage error, or the server refused the request. |
| `3` | Unanswerable: the selector was resolved against a partial view of the fleet (a federation satellite was unreachable). Retry once the link is back — unlike 1, the target may exist. |
| `124` | `phux wait` gave up because `--timeout` expired. |
| `125` | `phux run` gave up because `--timeout` expired; otherwise `run` mirrors the exit code of the command it ran, so `phux run … && next` composes like a shell. |

`phux run` exits with the child command's own code on success, so `phux run … && next` composes like a shell. A `--help` or `--version` request exits `0`, including when the reader hangs up early (`phux --help | head`).
