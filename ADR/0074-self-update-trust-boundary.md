---
audience: contributors
stability: stable
last-reviewed: 2026-08-07
---

# 0074 — The self-update trust boundary

**TL;DR.** `phux update` downloads a release, verifies it against the
published `.sha256` **before** unpacking, and replaces the binary by an atomic
`rename` within the destination directory, preserving the existing file's
mode. It never executes downloaded content to decide whether to install it,
never mutates an install another tool owns (Homebrew, Cargo, Nix), and refuses
an unrecognized location rather than overwriting it.

Status: Accepted
Date: 2026-08-07

## Context

[ADR-0071](./0071-what-phux-1-0-commits-to.md) makes the *release* the
compatibility unit: the wire keeps its own `0.x` line under
[ADR-0061](./0061-capabilities-add-versions-break.md), where a minor bump is a
fleet-wide break with no grace window and mismatched peers refuse each other at
HELLO. One deployment — server, local clients, satellites, relays — therefore
runs one release, and a fleet that cannot be moved in one step is a fleet that
will sit on a mismatch. ADR-0071 puts a one-command update path in 1.0 scope
for exactly that reason.

Before this, `phux upgrade` asked a running server to re-exec whatever binary
was already on disk. It discovered nothing, downloaded nothing, and verified
nothing; putting the new binary there was the user's problem, spread across
Homebrew, curl-installed tarballs, `cargo install`, and Nix.

A command that replaces the binary the user is running is a privileged
operation with a small blast radius and a large downside. The design space
that has to be closed off is not "how do we download a file" but "what are we
allowed to trust, and what are we allowed to touch".

## Decision

1. **The checksum is the trust anchor.** `release.yml` publishes
   `phux-<tag>-<target>.tar.gz` plus a `.sha256` sidecar. `phux update`
   computes the digest of the downloaded archive in-process and compares it to
   the sidecar before `tar` is ever pointed at the file. A mismatch refuses
   with both digests named, installs nothing, and exits non-zero. The sidecar's
   filename is checked too, so crossed release assets are caught.

2. **Nothing downloaded is executed to decide whether to install it.** The
   archive's member list is validated against the exact set a phux release
   contains, then it is unpacked, then the extracted tree is validated again
   (no symlinks, no non-regular files, no extra members). The one place a new
   binary is *run* is the server's pre-existing pre-commit `--version` check in
   `phux-server/src/runtime/upgrade.rs`, which happens after installation, on
   the server side, where a failure is harmless because nothing has been closed
   yet.

3. **Replacement is atomic and permission-preserving.** Staging happens in a
   directory created *beside* the target — same directory, therefore same
   filesystem, therefore `rename(2)` is atomic — and the staged file is given
   the mode of the file it replaces before it moves. A partially written binary
   is never reachable at the destination path, a restrictive mode survives, and
   a setuid bit smuggled into a tarball does not.

4. **Rollback is a file the user can reach.** The previous binaries are hard-
   linked into `<bindir>/.phux-update-backup/` with a JSON manifest naming the
   version. `phux update --rollback` renames them back. Because they are
   ordinary files in an ordinary directory, `mv .phux-update-backup/phux ./phux`
   is a complete manual recovery when the installed binary is too old to have
   the verb.

5. **Only an install phux maintains is mutated.** The install source is decided
   from the *symlink-resolved* path of the running executable. Nix store paths,
   Homebrew Cellars, and `$CARGO_HOME/bin` are recognized and never written to;
   each gets the exact native command instead. Direct-release installs are
   recognized by a documented allowlist of bin directories
   (`$PHUX_INSTALL_DIR`, `~/.local/bin`, `~/bin`, `/usr/local/bin`,
   `/opt/phux/bin`). Anything else is `unknown` and is refused.

6. **`phux upgrade` stays the primitive.** It re-execs what is on disk and
   downloads nothing. `phux update` puts the binary there and then calls it.

## Why

Detecting the install source positively and refusing everything else is the
only rule that fails safe. The tempting default — "if it is not Homebrew or
Nix, overwrite it" — silently corrupts anything the heuristic has not met:
a symlink farm, a `mise` shim, a distro package. Making an unrecognized layout
a refusal costs one error message and an environment variable to override; the
opposite error costs a user their `phux`.

Verifying before unpacking rather than after is what makes `tar` a consumer of
trusted bytes instead of an attack surface. Unpacking first and hashing after
would already have written attacker-chosen paths to disk.

Not executing the download to validate it is the one place this design refuses
a genuinely popular convention (rustup and friends smoke-test the new binary).
The reason is that the smoke test cannot distinguish "this binary is fine" from
"this binary did something and reported fine", and phux already has a strictly
better check in the server's pre-commit `--version` gate, which runs at the
moment where a failure costs nothing.

Doing the transport with `curl`/`wget` rather than linking an HTTP client keeps
TLS verification, proxy handling, and redirect following in implementations
that are audited and configured system-wide, and adds no dependency to a
terminal multiplexer. The documented install path already requires one of them.

## Tradeoffs

The checksum is served from the same host as the archive over the same TLS
channel, so it is not an independent trust root: it catches truncation,
corruption, crossed assets, and a mid-flight proxy, not a compromised GitHub
release. Real independence needs signing (minisign/cosign) with a key
distributed out of band, which is a release-machinery change, not a client
change. This ADR does not claim more than the mechanism delivers.

The direct-release allowlist will not recognize every reasonable layout, and
someone will be refused where an overwrite would have been correct.
`PHUX_INSTALL_DIR` is the release valve, and the refusal names it.

Shelling out to `curl`/`wget`/`tar` means three host tools whose absence is a
runtime failure rather than a compile-time one. The failure is loud and names
the missing tool, and the same three tools are already required by
`scripts/install.sh`.

## Alternatives

**Link an HTTP client (`reqwest`/`ureq`) and a `tar` crate.** Self-contained
and testable without host tools, at the cost of a large dependency subtree
(TLS stack, async runtime bridging, decompression) in a binary that has
carefully avoided one, for a code path most users run a handful of times a
year. Rejected on dependency cost; revisit if the checksum ever becomes a
signature and the verification moves in-process anyway.

**Sign the artifacts and verify a signature instead of a digest.** Strictly
better and the natural successor. It needs a signing key, a distribution story
for the public half, and a rotation policy — release machinery this ADR does
not decide. Deferred, with the checksum path shaped so a signature check drops
in ahead of it.

**Run the package manager for the user** (`brew upgrade`, `nix profile
upgrade`). Convenient, but it makes phux a wrapper around tools whose failure
modes and prompts it does not control, on installs it deliberately does not
own. Printing the exact command keeps the boundary where the rest of this ADR
puts it.

**Update in place with a copy instead of a rename.** Simpler and cross-device,
but a copy has a window in which the destination holds a truncated binary. The
whole point of staging beside the target is to close it.
