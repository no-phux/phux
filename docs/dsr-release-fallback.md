---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-08-07
---

# dsr: local release fallback

**TL;DR.** `dsr` replays this repo's existing `release.yml` locally — Linux
legs under `act`/Docker, the macOS leg on bare metal — and uploads the
resulting artifacts to the GitHub release with `gh release upload`. It is an
operator-invoked fallback for when GitHub Actions itself is throttled or
queued, not a second release lane: nothing in this repo changes for it to
work, and it never touches crates.io or the Homebrew tap.

## Why this exists

GitHub Actions minutes are billed and, at high enough queue pressure, throttled
independently of whether a workflow file is correct. When that happens,
`release.yml` can sit queued against a tag that release-please already
created, with no assets attached to the release and no useful signal beyond
"waiting." `dsr` ("doodlestein self-releaser",
[phall1/doodlestein_self_releaser](https://github.com/phall1/doodlestein_self_releaser),
forked from
[Dicklesworthstone/doodlestein_self_releaser](https://github.com/Dicklesworthstone/doodlestein_self_releaser))
exists to unblock exactly that case: it reads a repo's own release workflow
and reproduces its build steps on a machine the operator controls, so a
release can finish without waiting on Actions capacity.

## What dsr does and doesn't do

dsr reuses [`release.yml`](../.github/workflows/release.yml) as the source of
truth for how to build. It does not maintain a parallel build definition, so
there is nothing to keep in sync in this repo — a change to the matrix,
target list, or build steps in `release.yml` is picked up the next time dsr
runs, with no corresponding edit needed here.

It is scoped to the same two things `release.yml`'s `build` job produces:
the `phux-<tag>-<target>.tar.gz` tarballs and their `.sha256` sidecars,
attached to the GitHub release release-please already created. Per
[`RELEASING.md`](./RELEASING.md)'s ownership table, dsr does not replace
either of the two things release.yml itself does *not* own:

- **crates.io.** Publishing `phux-protocol` stays on the separate,
  human-gated `publish-crate.yml` dispatch. dsr has no path to it.
- **The Homebrew tap.** `release.yml`'s `homebrew` job regenerates and
  pushes `Formula/phux.rb` to `phall1/homebrew-tap` as part of the normal
  run. dsr does not run that job. If a release is finished entirely through
  dsr, the tap update is a manual follow-up:
  `bash scripts/gen-formula.sh` (see [`RELEASING.md`](./RELEASING.md#required-secret)
  for what it needs), then push the generated formula to the tap by hand.

## Running it

dsr is invoked by hand, or via `dsr watch --auto-fallback` if the operator
has set that running — either way it is not a persistent listener wired into
this repo's CI. From the operator's machine, with dsr already installed and
this repo registered in dsr's local config:

```sh
dsr check phall1/phux                      # is a release run currently throttled?
dsr build phux --targets linux/amd64       # build one or more targets locally
dsr release phux --version vX.Y.Z          # upload already-built artifacts to the release
dsr fallback phux --version vX.Y.Z         # check, build every configured target, and release, in one shot
```

`dsr build` and `dsr fallback` refuse to build from a dirty working tree
unless `--allow-dirty` is passed — a fail-closed guard against silently
packaging uncommitted local changes into a tagged release artifact, not a
bug to route around.

## Configuration lives outside this repo

Which targets dsr builds for phux, and how — `act` for Linux legs mapped
against release.yml's matrix, native build on which macOS host — is
operator-local configuration under `~/.config/dsr/`, not part of this
repository. Nothing here needs to reference secrets or specific hosts; from
this repo's side, dsr is just another way `release.yml`'s build steps get
run.

## See also

[`RELEASING.md`](./RELEASING.md) owns the release process itself: what
release-please does, what `release.yml` does, and what ships where. Read it
first if you're unsure whether a given step belongs to release-please,
release.yml, dsr, or a human.
