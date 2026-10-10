# Changelog

## [0.1.0-alpha.11](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.10...desktop-v0.1.0-alpha.11) (2026-10-10)


### Bug Fixes

* **desktop:** group the sidebar by the stored project tag ([#1160](https://github.com/no-phux/phux/issues/1160)) ([69bfa7a](https://github.com/no-phux/phux/commit/69bfa7a4f637a6810940684096ca58848b122e63))

## [0.1.0-alpha.10](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.9...desktop-v0.1.0-alpha.10) (2026-10-09)


### Bug Fixes

* **desktop:** fall back to Paper Mono when the font family is missing ([#1142](https://github.com/no-phux/phux/issues/1142)) ([8f4dfa8](https://github.com/no-phux/phux/commit/8f4dfa857b4984ab73c2974e554a914919b2d8d8))

## [0.1.0-alpha.9](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.8...desktop-v0.1.0-alpha.9) (2026-10-09)


### Features

* **clients:** session menus, directory terminals, and a folder on create ([#1115](https://github.com/no-phux/phux/issues/1115)) ([c6b2ff2](https://github.com/no-phux/phux/commit/c6b2ff20be7efb25aa9b28dd93d8434160f0d4cc))
* **clients:** share session host, directory, and project grouping ([#1108](https://github.com/no-phux/phux/issues/1108)) ([0f0e081](https://github.com/no-phux/phux/commit/0f0e0813e39b17541236ad9f988352e522b0ac7d))

## [0.1.0-alpha.8](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.7...desktop-v0.1.0-alpha.8) (2026-10-08)


### Bug Fixes

* **desktop:** spawn new tabs and splits in the focused pane's session ([cad1def](https://github.com/no-phux/phux/commit/cad1defea45666356e53662ef4e94284aa784de5))

## [0.1.0-alpha.7](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.6...desktop-v0.1.0-alpha.7) (2026-10-08)


### Features

* **ffi:** preserve unsupported agent-state vocabulary across projections ([99a9e03](https://github.com/no-phux/phux/commit/99a9e030aef8dac473559dde96474a76a7d98dcd))
* **workload:** devices without ssh enroll with a single-use ticket over their own ALPN ([f7d3cc7](https://github.com/no-phux/phux/commit/f7d3cc7f86dfe1edafefd185c9d78f19004d512a))

## [0.1.0-alpha.6](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.5...desktop-v0.1.0-alpha.6) (2026-10-07)


### Bug Fixes

* **desktop:** reject mixed and stale Rust toolchains before builds ([a0876b3](https://github.com/no-phux/phux/commit/a0876b3260a16e0de56533581dcdb16c66a18ce7))
* **dev:** run the Nix shell's pinned toolchain ahead of rustup's proxies ([df4185a](https://github.com/no-phux/phux/commit/df4185ab698b95d7a0acd5cf70724a9156e680f6))


### Build System

* **deps:** move libghostty onto the 2026-10-05 Ghostty rebase ([384558c](https://github.com/no-phux/phux/commit/384558c5757bda0b8ed90da7b100c457ac91b373))
* **deps:** pin libghostty-rs that keys shared builds on the Zig binary ([ba8897e](https://github.com/no-phux/phux/commit/ba8897e631184980f4ef41d613da18c850f09bf7))
* **deps:** pin libghostty-rs with shared, reproducible vendored builds ([f640042](https://github.com/no-phux/phux/commit/f640042e36c43ec18f3760d7eab5434cedd7fe8b))
* **deps:** pin libghostty-rs with trimmed shared builds ([f748d63](https://github.com/no-phux/phux/commit/f748d63fa72f5d3be2e46fd5b889fecca0af247a))

## [0.1.0-alpha.5](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.4...desktop-v0.1.0-alpha.5) (2026-10-02)


### Bug Fixes

* **desktop:** fence agent labels and notifications across drains ([ed9a544](https://github.com/no-phux/phux/commit/ed9a544b26524a6f66d024b72f13cb121d3ead49))
* **desktop:** preserve visible focus and retire stale agent badges ([22af75b](https://github.com/no-phux/phux/commit/22af75b4bf05d47cf80500f9716e7fcb4c390757))


### Performance

* **desktop:** instrument the host, reuse scenes, scope and pace repaints ([#980](https://github.com/no-phux/phux/issues/980)) ([04e3826](https://github.com/no-phux/phux/commit/04e38260cf69a4f0c5b2e243b198dddc56b9bb9a))

## [0.1.0-alpha.4](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.3...desktop-v0.1.0-alpha.4) (2026-10-02)


### Documentation

* **desktop:** pin only published alpha releases ([#966](https://github.com/no-phux/phux/issues/966)) ([6f1ab3d](https://github.com/no-phux/phux/commit/6f1ab3d49ee93278ee829147e7026fa898091a6d))

## [0.1.0-alpha.3](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.2...desktop-v0.1.0-alpha.3) (2026-10-02)


### Bug Fixes

* **desktop:** build native host with macOS system bash ([#964](https://github.com/no-phux/phux/issues/964)) ([725d39f](https://github.com/no-phux/phux/commit/725d39f0b09148b0a33043fe7f2dbe8d27a5be25))

## [0.1.0-alpha.2](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.1...desktop-v0.1.0-alpha.2) (2026-10-02)


### Bug Fixes

* **desktop:** leave generated changelog formatting to release-please ([#962](https://github.com/no-phux/phux/issues/962)) ([0b36207](https://github.com/no-phux/phux/commit/0b362078d3679906ea25c0e5ab20be5ebebb8509))

## 0.1.0-alpha.1 (2026-10-02)


### Features

* **desktop:** acknowledge unknown delivery from a Metal present receipt ([957a434](https://github.com/no-phux/phux/commit/957a43410b7174d28a847e6890614b2ec5db6a9b))
* **desktop:** add Ghostty jump_to_prompt, select_all, clipboard and write_*_file actions ([428a9d1](https://github.com/no-phux/phux/commit/428a9d1b86e8340de3fd6375baece769db4aa8a9))
* **desktop:** add the terminal shortcuts a daily driver actually hits ([856d826](https://github.com/no-phux/phux/commit/856d8264f53093960ad9036c3c81db44ba5f11d7))
* **desktop:** adopt the user's Ghostty config, windows and quick terminal ([245dd90](https://github.com/no-phux/phux/commit/245dd90e5e2f196c04443a69966ee43783346c56))
* **desktop:** connect host path picker to NAPI answers and pane routes ([791d071](https://github.com/no-phux/phux/commit/791d0714d3c483acac3e0e11118c9100d677f272))
* **desktop:** daily-driver fixes and more Ghostty keybind actions ([#896](https://github.com/no-phux/phux/issues/896)) ([edd7af2](https://github.com/no-phux/phux/commit/edd7af289bc090293df1e0480f5041eb837c31ff))
* **desktop:** fit panes to their PTY and draw the window chrome natively ([677453b](https://github.com/no-phux/phux/commit/677453b168a6c2f852d0a5bdc0515a4a058411ad))
* **desktop:** focus the active terminal and step through search ([85fed31](https://github.com/no-phux/phux/commit/85fed315bc7c26b468c999d4341ff657e63fa652))
* **desktop:** highlight the focused pane and follow live with Command-L ([25d4e44](https://github.com/no-phux/phux/commit/25d4e44bc74d85d124f00fd43b2b2f47d20aea23))
* **desktop:** land the Solid native terminal on Apple silicon ([4248754](https://github.com/no-phux/phux/commit/4248754a47cff676bfb315b97de69386bec3ce5f))
* **desktop:** let the shell ask a terminal for clipboard and open requests ([1ca62a5](https://github.com/no-phux/phux/commit/1ca62a5af252d9bb79483e077eafd1289cebf5ce))
* **desktop:** links, ANSI palette themes, app chords and global hotkeys natively ([0f345a8](https://github.com/no-phux/phux/commit/0f345a838808e36195f83b870e799bcdcad80c00))
* **desktop:** rebuild the Solid shell into a daily-driver terminal ([bb6a7ae](https://github.com/no-phux/phux/commit/bb6a7ae733a8da980584ea760b888ba84a988550))
* **desktop:** run the GPUIX app from a fresh checkout ([548b417](https://github.com/no-phux/phux/commit/548b4172a1fbf754fbb2e349db1d80acdefb5854))
* **desktop:** ship Phux.app for daily use ([1aa7acb](https://github.com/no-phux/phux/commit/1aa7acb4579381633221d86c52354a458485387d))
* **desktop:** ship qualified macOS alpha releases and installer ([#960](https://github.com/no-phux/phux/issues/960)) ([afd76b8](https://github.com/no-phux/phux/commit/afd76b853706a17a0741957379056f9d1b4420e0))
* **desktop:** show agent badges, settings, IME preedit, and a second window ([82a9722](https://github.com/no-phux/phux/commit/82a97227689149b4647da23c1d99d39116e9f382))
* **desktop:** show the daily shortcuts in the toolbar ([63f8e81](https://github.com/no-phux/phux/commit/63f8e81fb0a2d5e3dbfa8608c82e0e84e5be5deb))
* **desktop:** track the IME candidate and keep display prefs ([b3963c7](https://github.com/no-phux/phux/commit/b3963c74425773aa3f3c23009b01390617a83e48))
* **ffi:** expose prompt jumps, select-all and document text to desktop and the C ABI ([52b1d74](https://github.com/no-phux/phux/commit/52b1d746045cc4e6f60fe4afdc29d8a0a9d3ca03))


### Bug Fixes

* **deps:** bump yanked yoke-derive to 0.8.4 and drop the temporary deny ignore ([4aa2b04](https://github.com/no-phux/phux/commit/4aa2b0416d8271e03fac0ab1026e06708612d3be))
* **desktop:** act on Enter in the palette, Insert Path, rename and find ([48a50d1](https://github.com/no-phux/phux/commit/48a50d1cbc1e7cc1c6fb0be0ebbf738cacb33282))
* **desktop:** attach just desktop-app to the running server by default ([389d37d](https://github.com/no-phux/phux/commit/389d37df5ba696aa9646eb1d1863de594afd0b4d))
* **desktop:** bind Ghostty keybinds named by W3C code or physical key ([a127445](https://github.com/no-phux/phux/commit/a127445b4a3b7c9fa45e15f87708fd9c061ba1d8))
* **desktop:** build the host from agent worktrees and refresh its lock ([049e634](https://github.com/no-phux/phux/commit/049e6343575fd0df860a2a68745d9e0951feb3c8))
* **desktop:** catch a root dependency the host lock never resolved ([8c2efb9](https://github.com/no-phux/phux/commit/8c2efb9e4f2a7dbfde2b8ad6dcac59713062d33f))
* **desktop:** create the desktop session before just desktop-app attaches ([1dffa0b](https://github.com/no-phux/phux/commit/1dffa0bf3391c5c2b98dcacfb1f63f46bb272afe))
* **desktop:** decode Ghostty text: byte escapes as UTF-8 ([1811ba3](https://github.com/no-phux/phux/commit/1811ba375ad870b5f66c38cac9d1ac40cc026860))
* **desktop:** deliver multi-line and large clipboard pastes ([e9b512e](https://github.com/no-phux/phux/commit/e9b512e6b721278392bbff7f5a2f4dfb2f80e7ae))
* **desktop:** keep cross-session panes on restore and list every tab chord ([df4d9d7](https://github.com/no-phux/phux/commit/df4d9d79bc9fb3c09f64a666ad6920ae6bb1ab13))
* **desktop:** keep keyboard focus when a terminal's input rebinds ([a86f1bd](https://github.com/no-phux/phux/commit/a86f1bd5b658f4128177a884b60ed2e108263d59))
* **desktop:** keep Select All working beside a find bar on another pane ([fc1d9ee](https://github.com/no-phux/phux/commit/fc1d9ee553d846f28d89d3ad7d32acf4fb5efcb7))
* **desktop:** let a Ghostty bind back to a built-in drop the earlier rebind ([09cfa7e](https://github.com/no-phux/phux/commit/09cfa7ec58ff95e86d879134b81c52f78bd824ad))
* **desktop:** make Command-W close the pane, not the window ([f81f558](https://github.com/no-phux/phux/commit/f81f558aea592498457e70cf583191f0956444cd))
* **desktop:** never type a Ghostty text bind into the shell behind a text field ([34261bb](https://github.com/no-phux/phux/commit/34261bbee9e2133a7e65ab4fc3c887661a539b86))
* **desktop:** preserve workspace recovery and isolate demos ([#950](https://github.com/no-phux/phux/issues/950)) ([769192e](https://github.com/no-phux/phux/commit/769192e60d5c5e345641f604f89341e05ebfdd99))
* **desktop:** read Ghostty names only from the tables' own keys ([fd2b126](https://github.com/no-phux/phux/commit/fd2b1260a6792de48f0c1263fb42f1b69acbd26e))
* **desktop:** refresh the host lock for phux-config's nix dependency ([ab495e4](https://github.com/no-phux/phux/commit/ab495e429a1f4db5eb844bc81c4b06655039843b))
* **desktop:** refresh the host lock for the sha2 0.11 bump ([b8f5280](https://github.com/no-phux/phux/commit/b8f5280ac2f6fe06bf5b65f3302d51799d5f4628))
* **desktop:** run Copy and Paste commands while the find bar has focus ([13f1df8](https://github.com/no-phux/phux/commit/13f1df857b37c7a2a68e2fb9870e8b0834047c35))
* **desktop:** show a command's own chord, not its alias ([cceaa36](https://github.com/no-phux/phux/commit/cceaa36fa7a856b6910edabf252847c4ab2e2907))
* **desktop:** spawn into the attached session on a fresh server ([32c1897](https://github.com/no-phux/phux/commit/32c189779f8a2b80180190a70584b617a4b4b395))
* **release:** keep the desktop host lock in step with release versions ([#855](https://github.com/no-phux/phux/issues/855)) ([ba1f1f9](https://github.com/no-phux/phux/commit/ba1f1f9a74c18316c3a5df4f1635ab63fcf6ef41))
* **runtime:** respect roster ownership across bindings ([b493043](https://github.com/no-phux/phux/commit/b493043ac6eab0559ca0b632125efb00110041b8))
* unblock main CI after the Solid native and desktop landings ([#854](https://github.com/no-phux/phux/issues/854)) ([aa3d4d6](https://github.com/no-phux/phux/commit/aa3d4d6e9ede1a1d676f1ad1bdff149fea539d7c))


### Performance

* **client-runtime:** report owner-thread apply round trips and publications ([031f824](https://github.com/no-phux/phux/commit/031f8249f7be6015af16ad3a1c5c4b7b9f196aa8))
* **desktop:** paint text in runs and backgrounds as row spans ([f75a3e8](https://github.com/no-phux/phux/commit/f75a3e8b6004a1b08a617da59c812ef90b79b288))
* **desktop:** read the topology only on wakes that can change it ([49fae6b](https://github.com/no-phux/phux/commit/49fae6bce0d01fd6f831c77e57e23145e5e16228))
* **desktop:** report painter and key-to-paint timings ([314cd08](https://github.com/no-phux/phux/commit/314cd081af8db5e482a73b8f6afe1e8621a32460))


### Refactors

* repo-wide cull of dead code, redundant tests, and hack patches ([#888](https://github.com/no-phux/phux/issues/888)) ([1f73694](https://github.com/no-phux/phux/commit/1f73694e73686cac06b9f27455cb061f0328f842))


### Documentation

* expose gpui desktop demo installation ([#955](https://github.com/no-phux/phux/issues/955)) ([1436e60](https://github.com/no-phux/phux/commit/1436e60afd92ba64e32b0e075b09e59ff4b99b5b))
* retain desktop bundle architecture note ([#957](https://github.com/no-phux/phux/issues/957)) ([2f0fe2b](https://github.com/no-phux/phux/commit/2f0fe2bdfadbe9a92e63f6a54e5a111eaea8cbb5))

## Changelog

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
