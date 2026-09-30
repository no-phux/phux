---
audience: contributors
stability: stable
last-reviewed: 2026-09-27
---

# 0142 — Host path queries do not change directory listings

**TL;DR.** Leave `LIST_DIRECTORY`'s directory-only wire contract intact.
Give host-side file and directory browsing and recursive fuzzy search their
own L3 request/reply pair and one bit in the second feature word.

Status: Accepted
Date: 2026-09-27

## Context

`LIST_DIRECTORY` drives the existing go-to-directory picker. Its rows are
one-component directory names, with a symlink-to-directory flag. Changing it
to return file paths would change the meaning of bytes existing consumers
already decode. Native file-path insertion also needs cross-directory search
on the host where the shell runs, including satellite hosts; local filesystem
enumeration in a remote client cannot supply it.

The original server feature `u32` is closed by ADR-0137. In particular
`0x80000000` is not an available bit.

## Decision

1. Allocate `PATH_QUERY` (`0x56`) and `PATH_RESULTS` (`0xD4`) under L3.
   One `recursive` byte distinguishes one-level browse from cross-directory
   fuzzy search. Both modes include files, directories, and symlinks. The
   reply carries typed absolute UTF-8 paths suitable for lossless insertion,
   the resolved root and parent, completion/warming/truncation status or a
   typed refusal. The codec refuses over 1024 rows before allocation.
2. Gate both frames and satellite routing on
   `HELLO_OK.server_caps.features_ext.PATH_QUERY = 0x00000001`, the first bit
   of the second trailing `u32` after the original features word. Do not
   advertise this bit before a server serves the full contract. The existing
   feature word and old `LIST_DIRECTORY` encodings do not change.
3. The queried server owns the filesystem read. A hub forwards an optional
   `host` to its named satellite per request and refuses routing failures;
   it never silently searches the hub instead. A client must quote the
   original path string for its shell and escape it for display separately.

## Tradeoffs

- A second frame pair costs two L3 tags but keeps legacy directory pickers
  byte-for-byte safe and enables one negotiated capability for all clients.
- Non-UTF-8 filesystem names are omitted, never lossy-converted to a path
  that would select a different file. Supporting them would need a separate
  byte-path contract and shell insertion rules.
- Warming results are explicitly not exhaustive; consumers may retry but
  must not infer nonexistence from a partial result.

## Alternatives

- Extend `DIRECTORY_LISTING` with files: rejected because old consumers
  interpret every returned name as an enterable directory.
- Client-side search: rejected for remote and satellite hosts; it sees the
  client's filesystem, not the shell's.
- Spend `0x80000000` in word 0: prohibited by ADR-0137; older clients must
  see the old feature word unchanged.

## Related

- [L3.md §4–5](../spec/L3.md) — host queries and byte contract.
- ADR-0108 — hub-routed host queries.
- ADR-0137 — server feature word extension.
