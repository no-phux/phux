# Changelog

## Unreleased

### Features

- Make session ownership explicit with readable **Runs on …** host chips,
  restrained per-host glass tints, and an active session/host titlebar label
  that remains visible with the sidebar hidden.
- Add an explicit configured ACP-agent launch picker through the titlebar,
  File menu, workspace glass menu and command palette, server-owned conversation
  commands and native permission dialogs. Keep terminal-attached conversations
  explicitly labeled, with independent draft/scroll preservation.
- Surface executing-host prerequisites and queued/completed/refused/unknown
  outcomes without silent installs, automatic approvals or terminal-key fallback.

### Bug Fixes

- Preserve an ACP conversation's editor, pending receipts and question drafts
  when a graceful server handoff recreates desktop layout placements. Rebind
  only to the same endpoint, parent, AgentSession and ACP activation; a
  replacement owner never inherits the old draft.
- Keep native draft editing and Stop available while a provider question blocks
  another prompt. Queued or ambiguous delivery never consumes or resends a draft.
- Keep New ACP Agent clicks out of the titlebar's window-drag region.
- Reveal a newly launched ACP guardian from its authoritative catalog identity
  before waiting for the resulting agent subscription. Native controls still
  require the exact returned AgentSession and activation.
- Restore the focused ACP guardian after a daemon handoff even when it is not
  the session's home terminal, retaining the original editor and draft.
- Reject raw terminal input and file drops for pending or retained ACP owners
  before filesystem access or remote upload, including during reconnects.

## [0.26.1](https://github.com/no-phux/phux/compare/desktop-v0.26.0...desktop-v0.26.1) (2026-10-17)


### Performance

* **desktop:** paint retained glyph sprites per row ([0038778](https://github.com/no-phux/phux/commit/003877827510b6f3373dc99377a90a926b9d57bc))

## [0.26.0](https://github.com/no-phux/phux/compare/desktop-v0.25.1...desktop-v0.26.0) (2026-10-17)


### Features

* **desktop:** stage updates while running and swap after exit ([f81cd3d](https://github.com/no-phux/phux/commit/f81cd3dc9dc3fce430a4840fcc6445e0731b5e00))

## [0.25.1](https://github.com/no-phux/phux/compare/desktop-v0.25.0...desktop-v0.25.1) (2026-10-17)


### Build System

* **deps:** link libghostty-vt-sys's verified prebuilt download ([d5839db](https://github.com/no-phux/phux/commit/d5839db78deb09a58cba4090af4ced592cb43ccf))

## [0.25.0](https://github.com/no-phux/phux/compare/desktop-v0.24.0...desktop-v0.25.0) (2026-10-16)


### Features

* **desktop:** clarify session hosts with subtle glass accents ([78a24a5](https://github.com/no-phux/phux/commit/78a24a5af7b96bc2f2c8a7e6fc96919d83d5c0d2))

## [0.24.0](https://github.com/no-phux/phux/compare/desktop-v0.23.0...desktop-v0.24.0) (2026-10-16)


### Features

* **desktop:** scroll scrollback smoothly by pixels ([31e4a52](https://github.com/no-phux/phux/commit/31e4a52b0df75895042d1f56b0512a7bdafae5d5))


### Bug Fixes

* **desktop:** repaint on wheel scroll and follow the pointer ([f4bffbf](https://github.com/no-phux/phux/commit/f4bffbf0fa65404c376949182100c3b24fc3baa8))


### Performance

* **desktop:** await global hotkey presses instead of polling ([f83d849](https://github.com/no-phux/phux/commit/f83d8499f5e713ca5b03a6d24e2f6e330239eec1))
* **desktop:** build the desk's palette and terminal settings once ([a8d09bd](https://github.com/no-phux/phux/commit/a8d09bd58f765e644591f0ea5e4adeca108654e4))
* **desktop:** keep repeated agent metadata off the chrome rebuild ([d63fefb](https://github.com/no-phux/phux/commit/d63fefbd1cbf65a20eb1dea315c9160ca19bae29))
* **desktop:** notify fleet observers only when work changes ([6433f52](https://github.com/no-phux/phux/commit/6433f522fcc4d65ed21a6cb215d0444b5798b4c8))
* **desktop:** read the organization replica in place ([ea38555](https://github.com/no-phux/phux/commit/ea38555241151d83872cb8308a7bae3a9667da60))
* **desktop:** shape terminal rows as one line and paint glyphs in one layer ([8e3dde0](https://github.com/no-phux/phux/commit/8e3dde0e5e6dc456f9c8cd1d8367d7c4dec09d03))
* **desktop:** skip unchanged motion frames and borrow sidebar rows ([ab08915](https://github.com/no-phux/phux/commit/ab089157037855ea10f6954477a38a702dae1156))
* **desktop:** wake the agent-age clock when a label changes ([5236c56](https://github.com/no-phux/phux/commit/5236c569addeadb7b6660990e67dea4fdc022ce6))

## [0.23.0](https://github.com/no-phux/phux/compare/desktop-v0.22.0...desktop-v0.23.0) (2026-10-16)


### Features

* **organize:** one-step organize, and one client transport (ADR-0170, 0171, 0172) ([#1416](https://github.com/no-phux/phux/issues/1416)) ([492e820](https://github.com/no-phux/phux/commit/492e8203f86e38c7da1a8070f44ee90bdf6ace54))

## [0.22.0](https://github.com/no-phux/phux/compare/desktop-v0.21.0...desktop-v0.22.0) (2026-10-16)


### Features

* **desktop:** make menus and palettes quiet by default ([#1473](https://github.com/no-phux/phux/issues/1473)) ([bea8c78](https://github.com/no-phux/phux/commit/bea8c7818699752204e9b19b94bd4b832c512b10))

## [0.21.0](https://github.com/no-phux/phux/compare/desktop-v0.20.1...desktop-v0.21.0) (2026-10-16)


### Features

* **desktop:** unify federated work and navigation ([#1470](https://github.com/no-phux/phux/issues/1470)) ([e65059d](https://github.com/no-phux/phux/commit/e65059d3a67a3b0d9f7850ca8e64047371bafc9b))

## [0.20.1](https://github.com/no-phux/phux/compare/desktop-v0.20.0...desktop-v0.20.1) (2026-10-16)


### Bug Fixes

* **desktop:** restore native held-key repetition ([#1465](https://github.com/no-phux/phux/issues/1465)) ([b20ca0f](https://github.com/no-phux/phux/commit/b20ca0f4af4dcf44dbfed02e536f08b10a572b53))

## [0.20.0](https://github.com/no-phux/phux/compare/desktop-v0.19.0...desktop-v0.20.0) (2026-10-16)


### Features

* **desktop:** extend glass styling and add side tabs ([#1461](https://github.com/no-phux/phux/issues/1461)) ([ce3c0f7](https://github.com/no-phux/phux/commit/ce3c0f7f061632d3328486a02377cbbf6f2a44a8))

## [0.19.0](https://github.com/no-phux/phux/compare/desktop-v0.18.0...desktop-v0.19.0) (2026-10-16)


### Features

* **agent:** support ACP-owned sessions across harnesses ([#1431](https://github.com/no-phux/phux/issues/1431)) ([2696d16](https://github.com/no-phux/phux/commit/2696d16a273f58da0d100dda61816f6a8d50f314))

## [0.18.0](https://github.com/no-phux/phux/compare/desktop-v0.17.0...desktop-v0.18.0) (2026-10-15)


### Features

* **desktop:** rank Go To matches by bounded frecency ([#1451](https://github.com/no-phux/phux/issues/1451)) ([caaa2e3](https://github.com/no-phux/phux/commit/caaa2e3689971c6c2c1ee9f09e59937ed2f7eaf6))

## [0.17.0](https://github.com/no-phux/phux/compare/desktop-v0.16.0...desktop-v0.17.0) (2026-10-15)


### Features

* **desktop:** prepare a bounded Unicode-safe Go To search ([#1445](https://github.com/no-phux/phux/issues/1445)) ([7d76576](https://github.com/no-phux/phux/commit/7d765769290845e9598c4bc4b305a770e99ed514))
* **desktop:** upload remote drops and paste the server path ([#1442](https://github.com/no-phux/phux/issues/1442)) ([7e80053](https://github.com/no-phux/phux/commit/7e800537e2ffc191f7bcbe4e99e432cc807cbb1d))

## [0.16.0](https://github.com/no-phux/phux/compare/desktop-v0.15.0...desktop-v0.16.0) (2026-10-15)


### Features

* **desktop:** show metadata facts for the highlighted Go To row ([#1444](https://github.com/no-phux/phux/issues/1444)) ([01ddef3](https://github.com/no-phux/phux/commit/01ddef32d530acf6fa552ad53d75709859164540))


## [0.15.0](https://github.com/no-phux/phux/compare/desktop-v0.14.2...desktop-v0.15.0) (2026-10-15)


### Features

* **desktop:** filter Go To by agent state ([#1438](https://github.com/no-phux/phux/issues/1438)) ([24180f2](https://github.com/no-phux/phux/commit/24180f28f18201d0cb48507666524014e3810565))

## [0.14.2](https://github.com/no-phux/phux/compare/desktop-v0.14.1...desktop-v0.14.2) (2026-10-15)


### Bug Fixes

* **desktop:** keep remote terminals from pasting local file paths ([#1437](https://github.com/no-phux/phux/issues/1437)) ([2f31362](https://github.com/no-phux/phux/commit/2f3136264d67e69764f934feded3bd9bddbc3860))

## [0.14.1](https://github.com/no-phux/phux/compare/desktop-v0.14.0...desktop-v0.14.1) (2026-10-15)


### Build System

* **nix:** link a Cachix-served libghostty-vt instead of building Zig ([#1434](https://github.com/no-phux/phux/issues/1434)) ([baebc3a](https://github.com/no-phux/phux/commit/baebc3abd3798fcfb3180336d0de8e55e698cebb))

## [0.14.0](https://github.com/no-phux/phux/compare/desktop-v0.13.1...desktop-v0.14.0) (2026-10-15)


### Features

* **desktop:** give workspace menu native glass and spring motion ([#1432](https://github.com/no-phux/phux/issues/1432)) ([a82e993](https://github.com/no-phux/phux/commit/a82e993636ef493c561ded53101323ab9a775b21))

## [0.13.1](https://github.com/no-phux/phux/compare/desktop-v0.13.0...desktop-v0.13.1) (2026-10-15)


### Bug Fixes

* **desktop:** bind terminal links to the painted frame ([#1427](https://github.com/no-phux/phux/issues/1427)) ([d51583d](https://github.com/no-phux/phux/commit/d51583d3f774ddcef197f0327fef7e9da3641cc0))

## [0.13.0](https://github.com/no-phux/phux/compare/desktop-v0.12.2...desktop-v0.13.0) (2026-10-15)


### Features

* **desktop:** modernize native command palette ([#1421](https://github.com/no-phux/phux/issues/1421)) ([60976bd](https://github.com/no-phux/phux/commit/60976bd1fb335e37210bd2b4cbb8ec352109ef03))


### Bug Fixes

* **desktop:** admit delayed fits only for the visible size owner ([#1423](https://github.com/no-phux/phux/issues/1423)) ([90416a7](https://github.com/no-phux/phux/commit/90416a7e0c6bb9ea5499fd45434c5e13e558174b))

## [0.12.2](https://github.com/no-phux/phux/compare/desktop-v0.12.1...desktop-v0.12.2) (2026-10-15)


### Bug Fixes

* **desktop:** isolate the project-window fixture from the test scheduler ([#1417](https://github.com/no-phux/phux/issues/1417)) ([33fcd17](https://github.com/no-phux/phux/commit/33fcd17726f493b6e22c987a712f91545f7f5c5f))

## [0.12.1](https://github.com/no-phux/phux/compare/desktop-v0.12.0...desktop-v0.12.1) (2026-10-15)


### Bug Fixes

* **desktop:** align menus and quiet workspace chrome ([#1418](https://github.com/no-phux/phux/issues/1418)) ([b17dad3](https://github.com/no-phux/phux/commit/b17dad3a38dbb882ef8be3f1795e9d43551c4210))

## [0.12.0](https://github.com/no-phux/phux/compare/desktop-v0.11.1...desktop-v0.12.0) (2026-10-15)


### Features

* **desktop:** smooth opening and unify the main menu ([#1413](https://github.com/no-phux/phux/issues/1413)) ([c30c413](https://github.com/no-phux/phux/commit/c30c413de4765ffd2c40e1f2bcba1a380cccfbff))

## [0.11.1](https://github.com/no-phux/phux/compare/desktop-v0.11.0...desktop-v0.11.1) (2026-10-15)


### Bug Fixes

* **desktop:** map pointer hits onto the painted cell ([2ea27b3](https://github.com/no-phux/phux/commit/2ea27b3a6fa0d6f27decb086b4a72e267bc3fa4b))

## [0.11.0](https://github.com/no-phux/phux/compare/desktop-v0.10.0...desktop-v0.11.0) (2026-10-15)


### Features

* **native:** read terminal omp conversations in gpui ([#1375](https://github.com/no-phux/phux/issues/1375)) ([2d267e7](https://github.com/no-phux/phux/commit/2d267e7a9ac331e91dab44afde9d7dddee64e0d7))

## [0.10.0](https://github.com/no-phux/phux/compare/desktop-v0.9.4...desktop-v0.10.0) (2026-10-15)


### Features

* **desktop:** add live native performance monitoring ([e60e87e](https://github.com/no-phux/phux/commit/e60e87e5c05be81c30c1ffb9e167b178f07b181f))

## [0.9.4](https://github.com/no-phux/phux/compare/desktop-v0.9.3...desktop-v0.9.4) (2026-10-15)


### Performance

* **runtime:** keep the last frame across reconnect and size the websocket send budget ([#1392](https://github.com/no-phux/phux/issues/1392)) ([b33f042](https://github.com/no-phux/phux/commit/b33f0426c1b661fd216df75739a032b47be96b5b))

## [0.9.3](https://github.com/no-phux/phux/compare/desktop-v0.9.2...desktop-v0.9.3) (2026-10-15)


### Performance

* **desktop:** skip owner turns for unchanged frames ([#1391](https://github.com/no-phux/phux/issues/1391)) ([a1e0f09](https://github.com/no-phux/phux/commit/a1e0f09f47e465fd910df7f469e77cc8cd8c07ce))

## [0.9.2](https://github.com/no-phux/phux/compare/desktop-v0.9.1...desktop-v0.9.2) (2026-10-15)


### Performance

* **runtime:** probe quiet links and echo keys before the round trip ([#1385](https://github.com/no-phux/phux/issues/1385)) ([ec9a265](https://github.com/no-phux/phux/commit/ec9a265e63322b12de8f39b441a9577d4892e72f))

## [0.9.1](https://github.com/no-phux/phux/compare/desktop-v0.9.0...desktop-v0.9.1) (2026-10-15)


### Performance

* **desktop:** measure GPUI redraws headlessly ([#1381](https://github.com/no-phux/phux/issues/1381)) ([61b3e8b](https://github.com/no-phux/phux/commit/61b3e8ba1453e32f9b5ba33ad3b7873cd5b598f7))

## [0.9.0](https://github.com/no-phux/phux/compare/desktop-v0.8.6...desktop-v0.9.0) (2026-10-14)


### Features

* **desktop:** switch the active host in place and place spawned panes sooner ([#1379](https://github.com/no-phux/phux/issues/1379)) ([dd1d231](https://github.com/no-phux/phux/commit/dd1d23164f2e857c673b42ea9e6fe158bbcfb733))

## [0.8.6](https://github.com/no-phux/phux/compare/desktop-v0.8.5...desktop-v0.8.6) (2026-10-14)


### Performance

* **desktop:** use sparse damage for pane redraws ([#1372](https://github.com/no-phux/phux/issues/1372)) ([9856e15](https://github.com/no-phux/phux/commit/9856e15315a433590c81ce3127f6856701acd516))

## [0.8.5](https://github.com/no-phux/phux/compare/desktop-v0.8.4...desktop-v0.8.5) (2026-10-14)


### Performance

* **runtime:** redial a settled session before sleeping ([#1368](https://github.com/no-phux/phux/issues/1368)) ([cfd65d6](https://github.com/no-phux/phux/commit/cfd65d610cabcb81ad99f216febc775fd4a1e84a))

## [0.8.4](https://github.com/no-phux/phux/compare/desktop-v0.8.3...desktop-v0.8.4) (2026-10-14)


### Bug Fixes

* **desktop:** improve terminal rendering and quiet the sidebar ([#1363](https://github.com/no-phux/phux/issues/1363)) ([305b1ef](https://github.com/no-phux/phux/commit/305b1efca31ac1252afe50bfbf413430cc07409f))

## [0.8.3](https://github.com/no-phux/phux/compare/desktop-v0.8.2...desktop-v0.8.3) (2026-10-14)


### Performance

* **desktop:** keep the sidebar and titlebar across terminal frames ([#1361](https://github.com/no-phux/phux/issues/1361)) ([e0d0780](https://github.com/no-phux/phux/commit/e0d07800ff49c972cf4556411fd424a1d47692a3))

## [0.8.2](https://github.com/no-phux/phux/compare/desktop-v0.8.1...desktop-v0.8.2) (2026-10-14)


### Performance

* **desktop:** cache pane views and paint ASCII as lines ([#1357](https://github.com/no-phux/phux/issues/1357)) ([ea2b689](https://github.com/no-phux/phux/commit/ea2b68925e98ebbd1776e836b8b13f0d2b803480))

## [0.8.1](https://github.com/no-phux/phux/compare/desktop-v0.8.0...desktop-v0.8.1) (2026-10-14)


### Bug Fixes

* **deps:** bump rustls to 0.23.46 for RUSTSEC-2026-0346..0349 ([#1352](https://github.com/no-phux/phux/issues/1352)) ([5b6738c](https://github.com/no-phux/phux/commit/5b6738cdeebce780213336a786906aa4d7b23d43))


### Performance

* **desktop:** stop rebuilding the window on terminal output ([#1353](https://github.com/no-phux/phux/issues/1353)) ([79eeb8d](https://github.com/no-phux/phux/commit/79eeb8d8f2597d9f4ccd61b69866b316a7a5e03e))

## [0.8.0](https://github.com/no-phux/phux/compare/desktop-v0.7.0...desktop-v0.8.0) (2026-10-14)


### Features

* **desktop:** remember recent project folders ([#1350](https://github.com/no-phux/phux/issues/1350)) ([0944244](https://github.com/no-phux/phux/commit/0944244005aedbab6328c2666aaab8f94cea292d))

## [0.7.0](https://github.com/no-phux/phux/compare/desktop-v0.6.1...desktop-v0.7.0) (2026-10-14)


### Features

* **organization:** distinguish Intake from the attention Inbox ([#1344](https://github.com/no-phux/phux/issues/1344)) ([d18a547](https://github.com/no-phux/phux/commit/d18a547fb709ea08c4b6a56772bb4d0c21e3185c))

## [0.6.1](https://github.com/no-phux/phux/compare/desktop-v0.6.0...desktop-v0.6.1) (2026-10-14)


### Bug Fixes

* **desktop:** explain a missing Next pointer instead of a curl 404 ([611c041](https://github.com/no-phux/phux/commit/611c0415a5a4686f5087b1c402c902316aaa6c9c))

## [0.6.0](https://github.com/no-phux/phux/compare/desktop-v0.5.0...desktop-v0.6.0) (2026-10-14)


### Features

* **desktop:** give the Updates settings a real status surface ([b0992b0](https://github.com/no-phux/phux/commit/b0992b073aeb21456473790466b231c05c213f7e))
* **desktop:** manage remote hosts from the sidebar ([393ab41](https://github.com/no-phux/phux/commit/393ab4187884b72a2b4c64884c8146e4ec793b7f))

## [0.5.0](https://github.com/no-phux/phux/compare/desktop-v0.4.1...desktop-v0.5.0) (2026-10-14)


### Features

* **desktop:** open durable checkout projects ([#1332](https://github.com/no-phux/phux/issues/1332)) ([f2c14df](https://github.com/no-phux/phux/commit/f2c14df1ddec0009e408ca1bcfe4ad524a2ff884))

## [0.4.1](https://github.com/no-phux/phux/compare/desktop-v0.4.0...desktop-v0.4.1) (2026-10-14)


### Bug Fixes

* finish the 2026-10-13 landing seams (org store, import, upload lane, docs) ([#1318](https://github.com/no-phux/phux/issues/1318)) ([2aff3b7](https://github.com/no-phux/phux/commit/2aff3b7ddbc22ba56ac656d55fa437557f59dd10))

## [0.4.0](https://github.com/no-phux/phux/compare/desktop-v0.3.2...desktop-v0.4.0) (2026-10-14)


### Features

* **desktop:** add automatic stable and next updates ([#1313](https://github.com/no-phux/phux/issues/1313)) ([e850563](https://github.com/no-phux/phux/commit/e8505635fd34adc7cb4e6e1e95598ab49380acbb))

## [0.3.2](https://github.com/no-phux/phux/compare/desktop-v0.3.1...desktop-v0.3.2) (2026-10-14)


### Bug Fixes

* **desktop:** cull default UI down to what needs a person ([#1307](https://github.com/no-phux/phux/issues/1307)) ([9609826](https://github.com/no-phux/phux/commit/96098266a889856eb3839e12ef64a3ef058b5206))
* **install:** one command, PATH that works, and a Latest that names the CLI ([#1308](https://github.com/no-phux/phux/issues/1308)) ([33bfd51](https://github.com/no-phux/phux/commit/33bfd5180329846180e6341989c21dcc9a32d990))

## [0.3.1](https://github.com/no-phux/phux/compare/desktop-v0.3.0...desktop-v0.3.1) (2026-10-14)


### Performance

* **clients:** cheapen idle echo, desktop paint, and TUI style runs ([#1303](https://github.com/no-phux/phux/issues/1303)) ([04b31b3](https://github.com/no-phux/phux/commit/04b31b3fd396916f567082369e778be117e9be5f))

## [0.3.0](https://github.com/no-phux/phux/compare/desktop-v0.2.0...desktop-v0.3.0) (2026-10-14)


### Features

* **desktop:** one notification feed behind a titlebar bell ([#1279](https://github.com/no-phux/phux/issues/1279)) ([7732e1d](https://github.com/no-phux/phux/commit/7732e1db279f849e66bda3ebde9e8f548e0e3f4c))

## [0.2.0](https://github.com/no-phux/phux/compare/desktop-v0.1.0...desktop-v0.2.0) (2026-10-14)


### Features

* **desktop:** glass chrome, headerless panes, pane motion and image paste ([#1249](https://github.com/no-phux/phux/issues/1249)) ([43d2133](https://github.com/no-phux/phux/commit/43d21332aaf421fe7fe9ffed39dd0735caf6c60b))


### Bug Fixes

* **desktop:** preserve terminal Tab input before focus traversal ([#1255](https://github.com/no-phux/phux/issues/1255)) ([d02aa56](https://github.com/no-phux/phux/commit/d02aa5631033c292ea046c77c0f3ea00de5f06ce))
* **desktop:** retain explicit Tab shortcuts in terminal context ([#1261](https://github.com/no-phux/phux/issues/1261)) ([03556d4](https://github.com/no-phux/phux/commit/03556d4627fadf631e109d42b6ab239923fbab63))

## [0.1.0](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.17...desktop-v0.1.0) (2026-10-14)


### Features

* **desktop:** ship the stable release channel ([#1253](https://github.com/no-phux/phux/issues/1253)) ([3ce9e2a](https://github.com/no-phux/phux/commit/3ce9e2a1936d6e99c2f14f66523f9038d087e9bf))

## [0.1.0-alpha.17](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.16...desktop-v0.1.0-alpha.17) (2026-10-13)


### Features

* **desktop:** organize inbox work with atomic moves and undo ([7103ad1](https://github.com/no-phux/phux/commit/7103ad1748d744a77324fdddd08db9b14b6506cf))
* **org:** kill a session server-side with SESSION_KILL ([c859088](https://github.com/no-phux/phux/commit/c859088f12b4aaf13f2c3977c510616a05957801))


### Refactors

* **server:** take OSC 7501, 133 and 9;4 from the engine, not byte scanners ([#1226](https://github.com/no-phux/phux/issues/1226)) ([d0d0867](https://github.com/no-phux/phux/commit/d0d0867eb751d761880be84160bc3a7d4e24b36e))

## [0.1.0-alpha.16](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.15...desktop-v0.1.0-alpha.16) (2026-10-13)


### Features

* resources, sessions, Inbox, and actors domain model ([#1191](https://github.com/no-phux/phux/issues/1191)) ([fa80d73](https://github.com/no-phux/phux/commit/fa80d7328d315b8d57fd5c45968f9936d8cbaa10))


### Bug Fixes

* **desktop:** qualify on displays that cannot confirm presentation ([#1221](https://github.com/no-phux/phux/issues/1221)) ([2f65bc2](https://github.com/no-phux/phux/commit/2f65bc2cff91e9315f165faa62513544a43befe7))


### Build System

* **deps:** pin libghostty-rs synced with upstream on ghostty 2026-10-13 ([#1220](https://github.com/no-phux/phux/issues/1220)) ([b09248f](https://github.com/no-phux/phux/commit/b09248fdac73fb31eb062da35d2adac1e6555177))

## [0.1.0-alpha.15](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.14...desktop-v0.1.0-alpha.15) (2026-10-13)


### Bug Fixes

* **desktop:** keep presentation decisions in qualification evidence ([#1218](https://github.com/no-phux/phux/issues/1218)) ([4f286e8](https://github.com/no-phux/phux/commit/4f286e862ec3f4609d35063ccf3ba2210103339b))

## [0.1.0-alpha.14](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.13...desktop-v0.1.0-alpha.14) (2026-10-13)


### Features

* **desktop:** native GPUI desktop replaces the Solid host ([#1211](https://github.com/no-phux/phux/issues/1211)) ([360c651](https://github.com/no-phux/phux/commit/360c651acb8810ac06127ede782b63fd29b87be3))
* **pair:** name the MagicDNS host in pair links and default the QR on ([#1200](https://github.com/no-phux/phux/issues/1200)) ([5a1310f](https://github.com/no-phux/phux/commit/5a1310f9962c2bb6b1e5e5698e57c009acaeeabb))


### Documentation

* **adr:** renumber the GPUI desktop decision to 0165 ([#1214](https://github.com/no-phux/phux/issues/1214)) ([5475aad](https://github.com/no-phux/phux/commit/5475aad4711b4e2a214c47f86bb2a5776b7bca91))

## [Unreleased]

### Features

* **desktop:** run Phux.app as a native GPUI executable, with no Solid host or NAPI addon

## [0.1.0-alpha.13](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.12...desktop-v0.1.0-alpha.13) (2026-10-11)


### Bug Fixes

* **desktop:** materialize demo CLI under checkout ([1539fbf](https://github.com/no-phux/phux/commit/1539fbf7a80b83d564f0a4b93c8b15ac8f657b7a))

## [0.1.0-alpha.12](https://github.com/no-phux/phux/compare/desktop-v0.1.0-alpha.11...desktop-v0.1.0-alpha.12) (2026-10-11)


### Features

* **desktop:** add glass terminal polish ([7ef60b2](https://github.com/no-phux/phux/commit/7ef60b2ad0286c8cb3f8a22c08950ca23fe1e281))


### Bug Fixes

* **desktop:** route enter and tab through terminal key input ([ae06be7](https://github.com/no-phux/phux/commit/ae06be7686b337b62494d107335b51b35faa2fd9))

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
