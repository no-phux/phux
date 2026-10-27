#!/usr/bin/env bash
# Stable desktop publication runs only after every required platform qualifies.
set -euo pipefail

sidecars=("$RUNNER_TEMP"/desktop-sidecar/phux-desktop-*.sha256)
test "${#sidecars[@]}" -eq "$EXPECTED_DESKTOP_SIDECARS"
test -f "$RUNNER_TEMP/desktop-sidecar/phux-desktop-${RELEASE_TAG#desktop-v}-macos-arm64.dmg.sha256"
for sidecar in "${sidecars[@]}"; do
    name="$(basename "$sidecar")"
    # Sign only the checksum the draft actually carries.
    gh release download "$RELEASE_TAG" --pattern "$name" --dir "$RUNNER_TEMP/desktop-draft"
    cmp "$sidecar" "$RUNNER_TEMP/desktop-draft/$name"
done
cat "${sidecars[@]}" > "$RUNNER_TEMP/desktop-sidecar/SHA256SUMS"
bash scripts/ci/sign-release.sh "${sidecars[@]}"
gh release upload "$RELEASE_TAG" "$RUNNER_TEMP"/desktop-sidecar/*.minisig \
    "$RUNNER_TEMP/desktop-sidecar/SHA256SUMS" --clobber

notes="$RUNNER_TEMP/desktop-release-notes"
gh release view "$RELEASE_TAG" --json body --jq '.body // ""' > "$notes"
marker='Desktop: Developer ID signed and Apple-notarized.'
if ! grep -Fq "$marker" "$notes"; then
    printf '\n%s\n\nmacOS: download phux-desktop-%s-macos-arm64.dmg, open it, and drag Phux.app onto Applications. The ZIP is for automatic updates. Apple silicon macOS 26 or later; Ubuntu 24.04 GNU/Linux x64 and arm64 (X11 and Wayland; release qualification uses X11 with lavapipe). Recent DMGs include the phux CLI and server; older releases may require the separate CLI installer (https://phux.sh/install). Existing CLIs and running sessions are not replaced.\n' "$marker" "${RELEASE_TAG#desktop-v}" >> "$notes"
fi
windows_marker='Windows x64: native desktop, WSL coordinator.'
if [[ "$WINDOWS_DESKTOP" == true ]] && ! grep -Fq "$windows_marker" "$notes"; then
    printf '\n%s\n\nInstall with the per-user PowerShell installer: https://raw.githubusercontent.com/no-phux/phux/main/scripts/install-desktop-windows.ps1 . The portable ZIP includes the same installer and the MSVC runtime. Requires an initialized WSL distribution with phux; signature verification requires minisign on Windows PATH or in WSL.\n' "$windows_marker" >> "$notes"
fi
gh release edit "$RELEASE_TAG" --notes-file "$notes" --draft=false --latest=false
