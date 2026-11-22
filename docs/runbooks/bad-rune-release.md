# A bad Rune release

What to do when a published Phux Rune build is broken. How Rune releases
are cut is in [Releasing](../releasing.md#phux-rune).

Clients refuse older versions, so nothing published can pull a bad build back
off a machine that took it. **The way out is forward.** Two checks stand in
front of that: qualification before upload, and from alpha.8 a launch check in
the updater, which has the new binary draw one headless frame on the user's GPU
before anything is swapped. Clients on alpha.7 and earlier have no such check:
Linux runs `--version`, which exits before any GPU work, and macOS runs nothing
from the new build. Neither check opens a real window on the user's machine, so
a crash there still ships.

1. **Stop it spreading.** Put the last good feed back, so clients that haven't
   taken the bad version stop being offered it. Do this first.

   ```sh
   good=0.1.N
   dir="$(mktemp -d)"
   gh release download "rune-v$good" --repo no-phux/phux --dir "$dir" \
     --pattern '*.zip' --pattern '*.tar.gz' --pattern '*.sha256'
   git show "rune-v$good:crates/phux-protocol/src/lib.rs" > "$dir/lib.rs"
   python3 clients/rune/scripts/feed.py build "$good" \
     "$(git rev-parse "rune-v$good^{commit}")" "$dir" "$dir/lib.rs" > "$dir/rune.json"
   gh release upload channels "$dir/rune.json" --clobber --repo no-phux/phux
   ```

2. **Say so.** Edit the bad release's notes to name the problem. Keep the tag
   and the release: versions are never reused, and the next tag must sort above it.
3. **Fix forward.** Ship the fix as a higher version through the normal steps.
   The feed moves to it by itself, and clients on the bad build update as usual,
   if they still start.
4. **A build that won't start can't update itself.** Those users reinstall:
   `curl -fsSL https://phux.sh/install | sh -s -- --rune` takes whatever the feed
   names, even an older version, and keeps their settings and sessions.

The first real update from each older client on macOS and Linux is the release
evidence nothing here can replace: record alpha.6 to the next version, with
Restart now, quit-install and the server handoff, before calling it good.
