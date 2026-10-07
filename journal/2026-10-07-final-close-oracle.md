---
audience: agents, contributors
stability: scratch
last-reviewed: 2026-10-07
---
# Final-close ordering oracle

This delegated unit owns tests of the natural-close handoff, without changing
production behavior. Its oracle observes wire frames from real tracked pumps
and terminal actors. The original lost-exit case failed before the handoff
with `close overtook the final screen` (root's recorded red run).

The test module covers a complete final batch that fits the mailbox, a streamed
batch larger than a capacity-one mailbox, a pump already blocked on an ordinary
gap replacement, a healthy subscriber closing independently of a blocked
subscriber, and resource DETACH after reap has retired its wire ID. The DETACH
case supplies the final snapshot through an explicit reply barrier and manually
polls the actual publisher pending against a full mailbox before registering
and cancelling it. This avoids relying on task scheduling or merely exercising
abort before first poll. The final-byte oracle concatenates chunks, so it works
when a seven-byte codec limit splits the marker.

The native-profile test decodes the opaque checkpoint through libghostty's
snapshot decoder at READY, and checks the recovered final terminal title before
RESOURCE_CLOSED. It tests checkpoint content as well as frame order. Decoder
validation is not a claim of cryptographic authentication.

Tests use pending polls, channel barriers, and actual queue pressure to establish
ordering. Three-second timeouts only turn hangs into failures; they are not
ordering mechanisms. Root owns compilation, execution, integration, and the
final validation record, to avoid competing Cargo work in the shared worktree.
