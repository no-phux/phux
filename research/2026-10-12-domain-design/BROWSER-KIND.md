---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-10-12
---

# Browser resource kind

**TL;DR.** A Browser resource is one page executed on its owning server, under that owner's policy. Profiles are configuration, not organization. Every grant is separate: observing, interacting, inspecting, file transfer, permissions, profile export. Navigation fences input so a delayed click cannot hit the next page. This kind ships after Terminal; it exists now so the generic Resource does not assume a terminal.

Conforms to [FULL-DESIGN.md](FULL-DESIGN.md) §05. Command and grant mechanics are in [CONTRACTS.md](CONTRACTS.md).

## 01. What one resource is

- One Browser resource is one page (one top-level browsing context) running on the owner.
- A profile (cookies, storage, credentials) is launch configuration. Several pages may share one profile; when they do, they share that state and are never shown as isolated.
- A popup the policy allows becomes a new Browser resource in the same session, with provenance pointing at its opener. A popup the policy forbids is refused.
- Terminating a resource closes that page only, not other pages on the same profile.
- Relaunch opens a new page activation at the resource's declared launch URL. It does not restore the previous document.
- Profiles are temporary unless declared persistent. A temporary profile promises nothing about cookies surviving.

A webpage embedded in a client is a client-local view, not a Browser resource. The UI distinguishes it from shared work.

## 02. Execution and views

- The page executes on its owner. A client never substitutes a local browser and calls it the same page.
- Views negotiate a codec: interactive stream, screenshot sequence, or status only. A client that can only show screenshots says so; it does not fake interactivity.
- Viewport geometry follows an explicit policy, as Terminal geometry does. Mounting a view does not resize the page for others.

## 03. Document fencing

Every navigation produces a new document generation ID. Input that depends on the document (clicks, typing into a field, form submit) carries the generation the client saw. If the page has navigated since, the input fails as stale.

```text
LiveTarget(browser) = (ResourceRef, activation_id, server_instance_id, document_generation)
```

## 04. Grants

| Grant | Allows |
|---|---|
| browser.observe | Mount views, read screenshots and page text. Authenticated pages can expose secrets, so this is never implied by session observe on a page with a persistent profile. |
| browser.interact | Navigate, click, type, scroll. |
| browser.inspect | DOM, console, network metadata. Never credentials or cookie values. |
| browser.files | Uploads and downloads. |
| browser.permissions | Approve camera, microphone, location, notifications. |
| browser.profile_export | Export profile state. Separate and owner-only by default. |

- Owner policy decides allowed destinations, including owner-local and private-network services.
- Content on a page never grants authority. A page, popup, or fetched document cannot approve anything.
- No renderer or automation path bypasses these checks.

## 05. Profiles and membership

Sharing a profile couples otherwise separate resources, so reuse needs the owner's authorization for that profile's authenticated state.

Moving a page to a session with different access does not carry profile rights along. If the destination's grants are incompatible, the move is refused, or the user explicitly creates a new resource with an isolated profile. Cookies are never silently copied, and the original identity never changes.

## 06. Acceptance cases

| Scenario | Required behavior |
|---|---|
| Open a Browser resource in the TUI | Kind, status, and fallback shown; no terminal input sent. |
| Page navigates before a delayed click arrives | Click refused as stale. |
| Client asks for downloads or a private-network URL without grants | Refused by the owner; no fallback path around it. |
| Pages in differently authorized sessions ask for the same authenticated profile | Refused, or replaced by an explicitly approved isolated page. |
| Owner has no browser driver installed | Create returns unsupported; terminals work normally. |

Browser acceptance requires a real browser execution and view path. A terminal test with a browser icon does not count.
