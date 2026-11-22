# LTS releases

How the `lts` channel is promoted and patched. The release pipeline it sits
beside is in [Releasing](../releasing.md).

`lts` is the line for installs that want fewer, vetted updates:
`curl -fsSL https://phux.sh/install | sh -s -- --channel lts`, or
`phux channel lts`. `channels/lts.json` names one stable release a
maintainer promoted; no release moves it on its own.

**Policy.**

- A release is promoted about monthly, from the stable releases of that
  month that passed the soak gate.
- The promoted minor line `X.Y` gets security fixes for 12 months after its
  promotion, as patch releases cut from an `lts/X.Y` branch. Other bug fixes
  are backported only when they are severe; features never are.
- [SECURITY.md](../../SECURITY.md) lists what is supported: the latest stable and
  the current LTS.

**Soak gate.** `soak.yml` runs nightly on the newest stable release, and by
dispatch on any tag (`gh workflow run soak.yml -f ref=vX.Y.Z`). It runs
`just soak` on the tagged tree: the e2e lane, then ten rounds of the guards
for the two incidents a long-lived server hit (every graceful-upgrade test,
including the descriptor-leak check for the fd leak across upgrades, #1956;
and the per-pane systemd scope that keeps oomd off the server, #2006, run
against a real user manager), then the stress storms. The result is a
`soak` commit status on the tag's commit; a newer failing run withdraws an
older pass.

**Promotion.** `gh workflow run lts.yml -f tag=vX.Y.Z`. It refuses a tag
that is not a published CLI release with every target's tarball, checksum
and signature, or whose commit has no passing `soak` status, then publishes
`lts.json` and its signature. Clients refuse to move backwards without a
`--version` pin, so promote forward; to withdraw a bad LTS, promote a fixed
patch.

**Backports.** For a security fix to a supported line:

1. Land the fix on `main` first, as usual, unless the code is gone there.
2. Create the branch once: `git switch -c lts/X.Y vX.Y.Z` (the newest patch
   of that line), and push it. Later fixes go on the same branch.
3. On a branch from `lts/X.Y`, `git cherry-pick -x <commit>`, bump the patch in
   `Cargo.toml`, run `cargo update --workspace` so every `Cargo.lock` records
   it, and add the `CHANGELOG.md` entry. Open the pull request against
   `lts/X.Y`.
4. Once it is merged and green, tag `vX.Y.(Z+1)` on that commit, soak it,
   publish it, and promote it with `lts.yml`.

The publish step is not automated yet: `ci` runs on pushes to `main` only,
`release.yml` publishes only commits with a green push `ci` on `main`, and it
marks what it publishes GitHub's Latest, which a backport must not take
(`--latest=false`). Shipping the first backport needs those three changes
first.
