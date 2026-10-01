# Changelog

## Unreleased

### Features

- Introduce the independent `desktop-v0.1.0-alpha.1` release train for the GPUIX desktop on Apple silicon macOS.
- Package a compiled `Phux.app` with its native addon, a checksummed ZIP, and an explicit installed-phux CLI prerequisite.
- Qualify the packaged app against an isolated same-checkout server before publication, including terminal rendering and session survival across client termination and relaunch.

### Bug Fixes

- Preserve saved splits, ratios and focus while terminals reconnect instead of saving a partial layout.
- Isolate layouts by server socket and session; preserve damaged snapshots and report I/O failures without crashing or overwriting unread state.
- Keep concurrent installers serialized with a kernel lock that releases on process death; failed replacements roll back without touching sessions.

### Distribution

- Initial alphas are ad-hoc signed, not Apple-notarized, and never replace the stable CLI release in GitHub's latest-release API.
