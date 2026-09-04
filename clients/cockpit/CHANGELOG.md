# Changelog

All notable changes to Phux Cockpit are documented in this file. The project
uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.16.2](https://github.com/no-phux/phux/compare/cockpit-v0.16.1...cockpit-v0.16.2) (2026-09-04)


### Bug Fixes

* **cockpit:** make glyph diagnosis evidence-safe ([#516](https://github.com/no-phux/phux/issues/516)) ([821427d](https://github.com/no-phux/phux/commit/821427d37fa847c65214705036f0cd37f94773d5))
* **cockpit:** pin symlink-safe file writes ([#518](https://github.com/no-phux/phux/issues/518)) ([731f2a1](https://github.com/no-phux/phux/commit/731f2a1d9ede8fd750f5f677d9b5c9c51c6861d1))


### Documentation

* **cockpit:** add local reference capture workflow ([#517](https://github.com/no-phux/phux/issues/517)) ([b45af11](https://github.com/no-phux/phux/commit/b45af119f6a675666f8e99fb731aceeeb900ca80))
* **cockpit:** describe monorepo ownership ([cae7b80](https://github.com/no-phux/phux/commit/cae7b8075988ea6ea5e52bee14f184e2ea3f94d7))


### Build System

* **cockpit:** compose the native client from one checkout ([df79d15](https://github.com/no-phux/phux/commit/df79d1596c0797772cb545ca3ceaacef851e6e94))

## [0.16.1](https://github.com/no-phux/phux-cockpit/compare/v0.16.0...v0.16.1) (2026-09-03)


### Bug Fixes

* **dev:** admit isolated workspace state writes ([#83](https://github.com/no-phux/phux-cockpit/issues/83)) ([d940a08](https://github.com/no-phux/phux-cockpit/commit/d940a08dabfae62271e12fc139e2b8a0355abf45))

## [0.16.0](https://github.com/no-phux/phux-cockpit/compare/v0.15.0...v0.16.0) (2026-09-03)


### Features

* **native:** add vertical divider parity ([#81](https://github.com/no-phux/phux-cockpit/issues/81)) ([0701685](https://github.com/no-phux/phux-cockpit/commit/0701685a416907459c5bd767e76a56430e43f99b))


### Bug Fixes

* **native:** schedule useful first frame before deferred work ([#82](https://github.com/no-phux/phux-cockpit/issues/82)) ([f182236](https://github.com/no-phux/phux-cockpit/commit/f182236e309e4510640a8193b23d2d0cf6427a37))
* **release:** make keyless fallback preserve draft assets ([#80](https://github.com/no-phux/phux-cockpit/issues/80)) ([47d7c4e](https://github.com/no-phux/phux-cockpit/commit/47d7c4e64b55e0ed75b904617c41eec7c601c16d))
* **switcher:** fence keyboard activation by stable id ([#78](https://github.com/no-phux/phux-cockpit/issues/78)) ([0fcad32](https://github.com/no-phux/phux-cockpit/commit/0fcad323e7414d4f0eba80a777b9b969d339b1b4))

## [0.15.0](https://github.com/no-phux/phux-cockpit/compare/v0.14.0...v0.15.0) (2026-09-03)


### Features

* **native:** give the TypeScript-core spike real shells and native pixels ([#71](https://github.com/no-phux/phux-cockpit/issues/71)) ([417dfa9](https://github.com/no-phux/phux-cockpit/commit/417dfa9128f464c6f47ff725c21a2f50c0d5d03c))
* **native:** land the TypeScript core seam with a real engine behind it ([#68](https://github.com/no-phux/phux-cockpit/issues/68)) ([38e957a](https://github.com/no-phux/phux-cockpit/commit/38e957a1364511f1ae887e8f6dbc53f9fe86ba6b))
* **native:** markup parity harness, engine-owned tab run, toolchain and paint baseline ([#72](https://github.com/no-phux/phux-cockpit/issues/72)) ([ff96a05](https://github.com/no-phux/phux-cockpit/commit/ff96a052985879f55263cdf6fea8e6c1702e85fd))
* **native:** pointer, search, copy, paste and bells behind the TypeScript seam ([#75](https://github.com/no-phux/phux-cockpit/issues/75)) ([c2767bc](https://github.com/no-phux/phux-cockpit/commit/c2767bce9377028c20d8eb1d501714f270adaf90))
* **native:** real switcher and settings surfaces in the TypeScript spike ([#74](https://github.com/no-phux/phux-cockpit/issues/74)) ([14f2978](https://github.com/no-phux/phux-cockpit/commit/14f297869349aaee10b0c241be53fb8364e64725))
* **native:** secondary windows in the TypeScript-core spike ([#76](https://github.com/no-phux/phux-cockpit/issues/76)) ([09ddf91](https://github.com/no-phux/phux-cockpit/commit/09ddf916a0a6c86e71ed23f1fa14cefbed21d2d6))


### Bug Fixes

* **build:** install the TypeScript toolchain before the SDK's own check ([#73](https://github.com/no-phux/phux-cockpit/issues/73)) ([09fe43c](https://github.com/no-phux/phux-cockpit/commit/09fe43cf5fd0e9eb02ec3fb65905e793351bcaaf))
* **phux:** pin durable late-server retry ([#77](https://github.com/no-phux/phux-cockpit/issues/77)) ([92be8e5](https://github.com/no-phux/phux-cockpit/commit/92be8e5f6fa1230dcf46f6456b3013fbbd60e323))

## [0.14.0](https://github.com/no-phux/phux-cockpit/compare/v0.13.1...v0.14.0) (2026-09-02)


### Features

* **phux:** configure local socket and session ([#67](https://github.com/no-phux/phux-cockpit/issues/67)) ([57076c9](https://github.com/no-phux/phux-cockpit/commit/57076c93f98fced3432a98a970ce9ce663584d27))
* **phux:** separate inventory from visible tabs ([#63](https://github.com/no-phux/phux-cockpit/issues/63)) ([6724bcc](https://github.com/no-phux/phux-cockpit/commit/6724bccb951c7a769c78485df416935202899b21))


### Bug Fixes

* **phux:** pin multi-pane attach support ([#69](https://github.com/no-phux/phux-cockpit/issues/69)) ([b73cae4](https://github.com/no-phux/phux-cockpit/commit/b73cae4bb720d097308bf8f4c5e062efb858f60b))
* **state:** preserve rejected workspace files ([#66](https://github.com/no-phux/phux-cockpit/issues/66)) ([275ef56](https://github.com/no-phux/phux-cockpit/commit/275ef56949c4bcb518cbb81a461d99dbbbda5d01))

## [0.13.1](https://github.com/no-phux/phux-cockpit/compare/v0.13.0...v0.13.1) (2026-09-02)


### Bug Fixes

* **release:** detect release PR head updates ([#64](https://github.com/no-phux/phux-cockpit/issues/64)) ([b81c16f](https://github.com/no-phux/phux-cockpit/commit/b81c16f65adefd72ce117ac070a7cf0be251580d))
* **release:** validate updated release PRs ([#61](https://github.com/no-phux/phux-cockpit/issues/61)) ([24ff643](https://github.com/no-phux/phux-cockpit/commit/24ff643391a0f1387e2923665a17300834aab125))

## [0.13.0](https://github.com/no-phux/phux-cockpit/compare/v0.12.1...v0.13.0) (2026-09-02)


### Features

* **phux:** attach running sessions without creating ([#58](https://github.com/no-phux/phux-cockpit/issues/58)) ([60c3e3e](https://github.com/no-phux/phux-cockpit/commit/60c3e3e3489659b5c8bcb5bed8c718d9081bf2cd))


### Bug Fixes

* **release:** attest the shipped Phux FFI ([#60](https://github.com/no-phux/phux-cockpit/issues/60)) ([ba90c37](https://github.com/no-phux/phux-cockpit/commit/ba90c37846f4fbff2f00cbd0b690723a8fa7abf0))

## [0.12.1](https://github.com/no-phux/phux-cockpit/compare/v0.12.0...v0.12.1) (2026-08-28)


### Documentation

* plan the TypeScript authoring migration ([#54](https://github.com/no-phux/phux-cockpit/issues/54)) ([3006060](https://github.com/no-phux/phux-cockpit/commit/300606075a6e896e1ad940b6f41943e8a4865cae))

## [0.12.0](https://github.com/no-phux/phux-cockpit/compare/v0.11.0...v0.12.0) (2026-08-25)


### Features

* establish durable work foundation ([#49](https://github.com/no-phux/phux-cockpit/issues/49)) ([8c4e2db](https://github.com/no-phux/phux-cockpit/commit/8c4e2dbc444616de34471b5904f0850849fd62a9))
* **phux:** switch server-owned sessions ([c47701c](https://github.com/no-phux/phux-cockpit/commit/c47701cbfb50e2f5616b9c34b078a246627419b3))
* **terminal:** lift shell capacity and polish settings tabs ([e02b018](https://github.com/no-phux/phux-cockpit/commit/e02b018d8751329d066f44c3354cc802540d436f))
* **terminal:** preview OSC 8 targets before opening ([cd4b271](https://github.com/no-phux/phux-cockpit/commit/cd4b271194b4886f1e5675e6eaeb3729c49e47ac))


### Bug Fixes

* **chrome:** mark selected side rail tab ([7e63fde](https://github.com/no-phux/phux-cockpit/commit/7e63fdea845f994161cfeec7fe0d52086cc8eaf2))
* **ci:** pin compatible Phux client FFI ([#53](https://github.com/no-phux/phux-cockpit/issues/53)) ([1b31b7f](https://github.com/no-phux/phux-cockpit/commit/1b31b7f7cd1daf026901dc83f7f2bf65dd09bb8c))
* **ci:** refuse dirty SDK raster sources ([f9829b4](https://github.com/no-phux/phux-cockpit/commit/f9829b43cbaaaef211ddaa30959bfd02bee01ff6))
* clear tab refusal only when capacity returns ([87fe8aa](https://github.com/no-phux/phux-cockpit/commit/87fe8aa49c23feadac0eb40e15a94037f8a55421))
* **guards:** store side rail proof without trailing space ([34ab58f](https://github.com/no-phux/phux-cockpit/commit/34ab58f5cb06f40b9bf3d2961127cf8385a941d8))
* keep tab chrome truthful at capacity ([d9c8cd3](https://github.com/no-phux/phux-cockpit/commit/d9c8cd3f983958ba2a7e10e553d1bc32994dd49d))
* make measurement populations truthful ([ac1b7ed](https://github.com/no-phux/phux-cockpit/commit/ac1b7edb0b2aec3777217f0f184784ff86174f98))
* make topology retries current and visible ([a06a5c4](https://github.com/no-phux/phux-cockpit/commit/a06a5c4a83b827ebf24ddd5ded17f1a104ceca24))
* reap forced stops and isolate equal refs ([ec095e2](https://github.com/no-phux/phux-cockpit/commit/ec095e2a22953bd1d0b1c71b2ea82056231ecf11))
* restore durable work authority boundary ([#51](https://github.com/no-phux/phux-cockpit/issues/51)) ([8083cec](https://github.com/no-phux/phux-cockpit/commit/8083cec734c9710adaa9574016a893e8a1c20868))
* retry failed topology writes ([163b13a](https://github.com/no-phux/phux-cockpit/commit/163b13a5b34055bf5473f5d9766291156e4b4441))
* scope tab refusal to its workspace ([43a2f04](https://github.com/no-phux/phux-cockpit/commit/43a2f0410b3545043ed2f5e1250eda858ab30f42))
* **tabs:** centralize coordinator closeability ([bea1fe4](https://github.com/no-phux/phux-cockpit/commit/bea1fe4c5aa857d2126e46b234421cf3be984d57))
* **terminal:** canonicalize previewed OSC 8 authority ([ad53063](https://github.com/no-phux/phux-cockpit/commit/ad530630ebdb7fac5514656b9b7273cf5c45f996))
* **terminal:** require rendered OSC 8 target evidence ([0af605f](https://github.com/no-phux/phux-cockpit/commit/0af605fdf38aa3899d6b724296801675177459dd))


### Refactors

* name shared save notice reserve ([315f58c](https://github.com/no-phux/phux-cockpit/commit/315f58c506f40a418ff0609e403247d89695da79))

## [0.11.0](https://github.com/phall1/phux-cockpit/compare/v0.10.0...v0.11.0) (2026-08-16)


### Features

* **render:** capture the app's real frames without a Screen Recording grant ([#41](https://github.com/phall1/phux-cockpit/issues/41)) ([75553fb](https://github.com/phall1/phux-cockpit/commit/75553fbeb16bd7255fd0e108cfe6794570f9c882))


### Bug Fixes

* **chrome:** measure the tab strip's trailing reserve against the row it actually gets ([#47](https://github.com/phall1/phux-cockpit/issues/47)) ([45d84dd](https://github.com/phall1/phux-cockpit/commit/45d84dd77fce55cefff4cdf811f00711de27d93f))
* **settings:** say so when the config file refuses the theme write ([#46](https://github.com/phall1/phux-cockpit/issues/46)) ([3f34235](https://github.com/phall1/phux-cockpit/commit/3f34235d9c69119fff9d4ff30572bbec812d3af1))
* **tabs:** elide a shrunken tab's title in the MIDDLE so the strip still names its tabs ([#48](https://github.com/phall1/phux-cockpit/issues/48)) ([f5d8534](https://github.com/phall1/phux-cockpit/commit/f5d853494883241c0972e4315d7d26ff04a7fd9c))
* **tabs:** scroll into view only when the selection is actually out of view ([#45](https://github.com/phall1/phux-cockpit/issues/45)) ([8884c55](https://github.com/phall1/phux-cockpit/commit/8884c551550a0709b7e3421f68b5e619a8bb952b))
* **worktree:** refuse a commit that lands in somebody else's worktree ([#44](https://github.com/phall1/phux-cockpit/issues/44)) ([3c7d9a3](https://github.com/phall1/phux-cockpit/commit/3c7d9a3e0b35b7d327a4e69be9c9f911799bab47))

## [0.10.0](https://github.com/phall1/phux-cockpit/compare/v0.9.0...v0.10.0) (2026-08-15)


### Features

* **terminal:** give the terminal a minimum-contrast floor ([b9b4885](https://github.com/phall1/phux-cockpit/commit/b9b488576eb8d431ed4a8fdb55015fc503f8d326))
* **terminal:** surface OSC 8 hyperlinks and underline a link on hover ([bb3b67d](https://github.com/phall1/phux-cockpit/commit/bb3b67d253c308a404dcb8c202ce23b4a7245a21))


### Bug Fixes

* **automation:** bind a live-app run to one pid, and refuse the rest loudly ([bbe3717](https://github.com/phall1/phux-cockpit/commit/bbe371786870c4c1e482b8806af3e205ac893ccc))
* **build:** give each worktree its own Zig global cache ([0dde91c](https://github.com/phall1/phux-cockpit/commit/0dde91cd351138e8543d1ec50e6d888c78a96b21))
* **build:** keep the isolation check from writing outside its build root ([55b54df](https://github.com/phall1/phux-cockpit/commit/55b54dfcc67c909ca0bf0314bce0ff7feb52746e))
* **guards:** break the deadlock a stale guard puts the mechanism in ([1ec72e8](https://github.com/phall1/phux-cockpit/commit/1ec72e80ce5ec091d2c110d0d1f3e33af5f1f3d1))
* **guards:** the guard scripts required bash 4, and CI runs bash 3.2 ([baa6b74](https://github.com/phall1/phux-cockpit/commit/baa6b7471c6d136a3f4000a3bd57f071e85635ea))
* **phux:** make writeExact's deadline reachable, and root extension.zig ([ae0ffa7](https://github.com/phall1/phux-cockpit/commit/ae0ffa7e746db3ec980642d45b1b77deeb01e914))
* **state:** write the layout before creating its directory ([68987a3](https://github.com/phall1/phux-cockpit/commit/68987a32ed2f2fc55b9ff0741d51dc7c1e246678))


### Documentation

* **claude:** replace the dev-run placeholder with the command that landed ([caea926](https://github.com/phall1/phux-cockpit/commit/caea9264117f2a80e8b864a799eced5397efd7c9))
* fill CLAUDE.md from what the repo now knows ([45faf08](https://github.com/phall1/phux-cockpit/commit/45faf08f4261abbf7fe4d1b94a6527fdca95b1d6))
* **render:** confirm the contrast floor on real composited pixels ([bff6209](https://github.com/phall1/phux-cockpit/commit/bff62097d2b4342d871f37734858cabb9ca13d11))
* **terminal:** name the error the compiler actually reports for Flattened.init ([c45ee9e](https://github.com/phall1/phux-cockpit/commit/c45ee9e34a3b7ee878a6713bb5a1cd8442d304a0))

## [0.9.0](https://github.com/phall1/phux-cockpit/compare/v0.8.0...v0.9.0) (2026-08-15)


### Features

* **cockpit:** catch up to native-sdk v0.9.0, and use what it added ([84db570](https://github.com/phall1/phux-cockpit/commit/84db5702c11129102bf44cec8a8dcac2833b77ee))
* **cockpit:** raise the pty ceiling from 4 concurrent shells to 32 ([#33](https://github.com/phall1/phux-cockpit/issues/33)) ([9debd99](https://github.com/phall1/phux-cockpit/commit/9debd99b75daaf7f75b45a3522e6a0c1926abf0d))
* **dev:** one command to build and run this checkout, unmistakable for the installed app ([#37](https://github.com/phall1/phux-cockpit/issues/37)) ([609c4fc](https://github.com/phall1/phux-cockpit/commit/609c4fc9c14e9e521eaaf2f818fefe44ef0a4b3c))
* **sdk:** update to upstream v0.9.0 ([#39](https://github.com/phall1/phux-cockpit/issues/39)) ([3eaddc2](https://github.com/phall1/phux-cockpit/commit/3eaddc21d826d60b03e39075172b02c31b39b9d9))


### Bug Fixes

* **chrome:** one register for every band, and an accent that says which ([d8d4541](https://github.com/phall1/phux-cockpit/commit/d8d4541736d6b66db6719388f8626b9bba2d7c5c))
* close the open bead backlog (15 of 18) ([#28](https://github.com/phall1/phux-cockpit/issues/28)) ([175aa1e](https://github.com/phall1/phux-cockpit/commit/175aa1e4d22e58d7b1b14f13ee1bb285e971452f))
* **metrics:** make the unmeasured cell representable, and stop sizing remote panes with a sans font ([#34](https://github.com/phall1/phux-cockpit/issues/34)) ([5bccbd2](https://github.com/phall1/phux-cockpit/commit/5bccbd264f25fbe8abea8f83339666dcf731321a))
* **render:** pin the SDK that actually paints terminal output ([#38](https://github.com/phall1/phux-cockpit/issues/38)) ([5b3d3fb](https://github.com/phall1/phux-cockpit/commit/5b3d3fb573cdafcffe5a713b8ff3a5977fad85da))
* **scripts:** the pin gate certified ghostty as the SDK, green ([#35](https://github.com/phall1/phux-cockpit/issues/35)) ([39cba80](https://github.com/phall1/phux-cockpit/commit/39cba806aad6ccb84c7f9adc1533631e7024cd47))


### Documentation

* **design:** write down the chrome register, with numbers and sources ([c890771](https://github.com/phall1/phux-cockpit/commit/c890771dc798b647126cf8f08fe5741652b586cb))

## [0.8.0](https://github.com/phall1/phux-cockpit/compare/v0.7.1...v0.8.0) (2026-08-09)


### Features

* **release:** report releases to the Linear phux-cockpit pipeline ([#24](https://github.com/phall1/phux-cockpit/issues/24)) ([358b24c](https://github.com/phall1/phux-cockpit/commit/358b24ca23a1f4356d4753bef734c28979b4cd15))


### Documentation

* close out the spike lineage and correct what it left behind ([#26](https://github.com/phall1/phux-cockpit/issues/26)) ([0c33d58](https://github.com/phall1/phux-cockpit/commit/0c33d58aa3cc2547418eed2da37fd1147d68a24b))

## [0.7.1](https://github.com/phall1/phux-cockpit/compare/v0.7.0...v0.7.1) (2026-08-09)


### Bug Fixes

* pin the SDK back to the v0.8.1 base, which restores keyboard input ([#22](https://github.com/phall1/phux-cockpit/issues/22)) ([e00b459](https://github.com/phall1/phux-cockpit/commit/e00b4599be2d704e499560e9c64ff4bb03339896))

## [0.7.0](https://github.com/phall1/phux-cockpit/compare/v0.6.1...v0.7.0) (2026-08-09)


### Features

* move the SDK pin to v0.8.3, soften the split scrim, and make automation drivable ([#20](https://github.com/phall1/phux-cockpit/issues/20)) ([3e825ea](https://github.com/phall1/phux-cockpit/commit/3e825eac9a2cc4a5e005ec6b7203f369fd810ad7))

## [0.6.1](https://github.com/phall1/phux-cockpit/compare/v0.6.0...v0.6.1) (2026-08-09)


### Bug Fixes

* stop a dev build from clobbering the installed app's saved layout ([632e584](https://github.com/phall1/phux-cockpit/commit/632e5846b3b5ece4c965e743a89808592bb4fc07))
* stop chrome resizing terminals, close dead panes, and redesign the tab strip ([#19](https://github.com/phall1/phux-cockpit/issues/19)) ([06c0208](https://github.com/phall1/phux-cockpit/commit/06c0208a35160363d110d788158ee89be5b52945))


### Documentation

* describe the dsr local release fallback ([#17](https://github.com/phall1/phux-cockpit/issues/17)) ([6dbe6a2](https://github.com/phall1/phux-cockpit/commit/6dbe6a28d80ba1018c9e29413f230654758adbe9))

## [0.6.0](https://github.com/phall1/phux-cockpit/compare/v0.5.0...v0.6.0) (2026-08-07)


### Features

* **cockpit:** add the recursive pane layout tree ([63e806a](https://github.com/phall1/phux-cockpit/commit/63e806a100cbcf50a2296c46125be90617723b54))
* **cockpit:** pack the terminal into one cell grid, and finish the chrome ([b7c7c08](https://github.com/phall1/phux-cockpit/commit/b7c7c08e7f1807d5e98976748694696081ff23f4))
* **cockpit:** rebuild layout, close semantics, chrome, and terminal fidelity ([20d82dd](https://github.com/phall1/phux-cockpit/commit/20d82dd58719e5294c4d72a99090426f6f7b5cc8))
* multiple windows, each with its own workspace ([74c9ca6](https://github.com/phall1/phux-cockpit/commit/74c9ca63afbebd36119bffb96b83ebc7c4dec185))
* restore the GPU path, persist the workspace, and make close mean close ([ff04874](https://github.com/phall1/phux-cockpit/commit/ff04874ea773cfccf05d87ff55afcd06b2171f11))
* scrollback search, and bold and italic that actually render ([814f447](https://github.com/phall1/phux-cockpit/commit/814f447aa3c496c0daa8581c927f99e824f9175d))
* **terminal:** carry every SGR attribute into the packed cell ([984fa38](https://github.com/phall1/phux-cockpit/commit/984fa381990951a569758d6436947548dfb7fed7))


### Bug Fixes

* repair the production phux provider build ([3544f4c](https://github.com/phall1/phux-cockpit/commit/3544f4c192f417e82c8ae64f8c009fc046ff9c06))
* retain the copied selection on remote panes ([2fb8927](https://github.com/phall1/phux-cockpit/commit/2fb8927eea3385018002d1f7cdf6458d8953da0d))


### Documentation

* describe the terminal that exists now ([d2ff2a2](https://github.com/phall1/phux-cockpit/commit/d2ff2a28172bf185b9fdc9c55ab0df16c5f503f9))
* rewrite the topology snapshot doc for pane trees ([c8bcb18](https://github.com/phall1/phux-cockpit/commit/c8bcb18b3aac4bbd365563c345633d5630e308b6))

## [0.5.0](https://github.com/phall1/phux-cockpit/compare/v0.4.0...v0.5.0) (2026-08-05)


### Features

* **cockpit:** polish terminal workspace ([#11](https://github.com/phall1/phux-cockpit/issues/11)) ([431b67c](https://github.com/phall1/phux-cockpit/commit/431b67cae91541350e2dc7aeb2be485b5a33841c))


### Refactors

* **cockpit:** organize source by ownership ([#12](https://github.com/phall1/phux-cockpit/issues/12)) ([b77e0e1](https://github.com/phall1/phux-cockpit/commit/b77e0e1e3734709172eb6184f4a4e8a75b8f6bea))

## [0.4.0] - 2026-08-04

### Changed

- Cockpit at rest is now a bare terminal. The tab and control band emerges only
  when the workspace has structure to show — a second terminal, a split, the Web
  surface, or a terminal needing attention — and retracts when it does not.
  Reveal is driven only by discrete state the operator caused, so nothing
  incidental reflows the content area or resizes a live PTY. Every control the
  band carried stays reachable by keyboard in every state, and the titlebar
  inset keeps the window draggable when the band is absent.
- Terminal surfaces now carry a self-sufficient accessibility label (identity,
  provider, and lifecycle) rather than relying on the tab above them.
- Phux terminals published by a coordinator now enter the same bounded tab
  topology as local terminals instead of claiming a visible placement on
  discovery. Reconciliation prunes placements whose remote terminal is gone and
  no longer evicts a live local terminal. `cmd+W` closes local terminals only.
- Topology snapshots persist local topology only, through a dedicated snapshot
  selection type; a remote terminal's existence belongs to its coordinator.

- Terminal tabs now expose hidden process failures, while compact status chrome
  prioritizes the active exception, preserves full diagnostic semantics, and
  distinguishes spawn rejection from spawn failure.
- Clean and abnormal exits now provide placement-specific Restart controls;
  `cmd+R` continues to restart the focused terminal.
- Current PTY input stalls clear after recovery; native delivery failures remain
  distinct from bytes confirmed lost in an application queue.
- Terminal pointer interaction now includes native Copy/Paste menus, persistent
  copied highlights, I-beam and text-value accessibility, edge autoscroll,
  protocol-fenced captures, and fair independent wheel accumulation.
- Secondary click has explicit mode ownership: a live mouse-reporting TUI gets
  raw down/up without AppKit menu tracking; local and ended terminals get the
  native Copy/Paste menu instead.

## [0.3.0] - 2026-08-02

### Added

- Native accessible tabs for two terminal surfaces and system WebKit.
- A real draggable and keyboard-operable terminal split with model-owned
  geometry, active-pane focus, and direct `cmd+D` control.
- Previous/next tab shortcuts that remain available while WebKit owns the
  native first responder, plus split-pane focus shortcuts on the terminal
  canvas.
- Combined two-terminal rendering budgets and adversarial coverage for IDs,
  geometry, input isolation, PTY resizing, and process-lifetime independence.

### Changed

- Cockpit no longer launches or embeds the phux TUI. Both terminal surfaces
  run ordinary login-configured interactive shells while the native
  control-plane protocol remains future work.
- Surface identity is independent from single/split placement; entering,
  resizing, focusing, and leaving a split preserves both live sessions.
- The Work rail has been replaced by a compact native tab and action band,
  returning the full window width to content.

## [0.2.0] - 2026-08-02

### Added

- A stable Work rail over Workspace, Scratch, and Web surfaces.
- A native system-WebKit research surface with allowlisted top-level origins,
  disabled native commands, and explicit app-owned root navigation.
- Product-level Work selection independent from terminal focus, including
  `cmd+1`, `cmd+2`, and `cmd+3` navigation.

### Changed

- The selected terminal now uses the full content area while hidden terminal
  executions continue ingesting output without reset or respawn.
- Pointer, keyboard, paste, restart, and wheel routing are surface-aware so a
  webview or rail interaction cannot leak into a hidden terminal.
- Test coverage now verifies Work transitions, hidden execution preservation,
  WebKit bindings, rail isolation, accessibility, and selected-surface budgets.

## [0.1.0] - 2026-08-02

### Added

- Interim macOS Companion with Workspace and Local Shell terminal panes in one
  native Metal window.
- Keyboard focus, selection, safe terminal-aware copy/paste, scrollback, and
  pane restart controls.
- A fixed dark graphite and lime Phux visual register with concise, accessible
  process and I/O-loss status.
- Native app identity, icon, ad-hoc local packaging, optional Developer ID
  signing and notarization, ZIP and DMG artifacts, and SHA-256 checksums.
- macOS CI and tag-driven GitHub release automation for Zig 0.16.0.

### Known limitations

- Workspace delegates to the installed phux TUI; this is not the future native
  `SessionKernel` client.
- Local Shell is ephemeral and is not a durable phux session.
- The release supports Apple silicon macOS only.

[0.4.0]: https://github.com/phall1/phux-cockpit/releases/tag/v0.4.0
[0.3.0]: https://github.com/phall1/phux-cockpit/releases/tag/v0.3.0
[0.2.0]: https://github.com/phall1/phux-cockpit/releases/tag/v0.2.0
[0.1.0]: https://github.com/phall1/phux-cockpit/releases/tag/v0.1.0
