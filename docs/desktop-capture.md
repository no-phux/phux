# Desktop Quick Capture

Quick Capture turns a thought into an interactive OpenCode terminal using the
connected server's existing spawn contract. It is a desktop launch workflow,
not a durable inbox or a cross-device capture service.

## Open and compose

Press **Cmd+Shift+Space inside Phux**, or choose Capture in the command palette
(`Cmd+P`, then `!`). This is an application-local shortcut, not a global
system-wide hotkey. Capture uses a retained native textarea: reopening the
sheet keeps the draft, selection, and undo history while the app is alive.

| Key | Result |
| --- | --- |
| Enter | Send, then follow the confirmed ready terminal into a new local tab. |
| Cmd+Enter on macOS / Ctrl+Enter elsewhere | Send and stay in Capture, without creating a local placement. |
| Shift+Enter | Insert a newline. |
| Dismiss | Close the sheet without throwing away the draft or editor state. |

The send-and-stay terminal is still interactive and discoverable through
server topology, the sidebar, and Go To. It is not a different headless mode.
A successful receipt clears the submitted text only if the user has not edited
it in flight. Refusal keeps the draft; pending sends suppress duplicate submits.

## Know the destination before sending

The destination preview uses the same validated resolver as launch. A leading
`@session` selects that session on the connected server only when it has one
distinct known local working directory. Without an explicit selector,
Capture uses the focused local pane's session and working directory. Unknown
or ambiguous targets and remote paths are refused; Capture never guesses another worktree or treats a
remote folder as local.

Text is passed as an argv argument (`opencode --prompt TEXT`), not interpolated
into a shell command. The spike supports OpenCode; other providers, names with
spaces, repo/pane selectors, remote forwarding, automatic worktree creation,
and delivery into an existing agent session are not this workflow.

## Receipts are not model admission

Following waits for the spawned terminal to be ready. A spawn receipt is not
proof that an agent is working or that a provider accepted the first prompt:
login, trust, and startup failures can still follow. Normal permission prompts
are preserved. Ambiguous delivery is never silently retried under a new identity.

The draft is not saved durably on the server. The proposed shared intake
record, mobile/TUI capture, cross-device synchronization, and durable dispatch
deduplication remain future work, not guarantees of the shipping composer.
The [historical decision](./architecture.md#historical-decisions) is ADR-0188.

## Implementation seam

`clients/desktop/native/src/app/capture.rs` owns draft, receipt and destination
state; `app/capture/composer.rs` renders the retained Kit editor. The regular
keymap catalog exposes the command. The existing server spawn, bootstrap and
placement seams own execution/readiness; Capture does not fabricate an
AgentSession or bypass the host's trust controls.
