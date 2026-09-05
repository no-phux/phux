# Changelog

## [0.2.3](https://github.com/no-phux/phux/compare/pi-extension-v0.2.2...pi-extension-v0.2.3) (2026-09-05)


### Bug Fixes

* **ci:** retry the npm audit gates on registry outage instead of failing ([87e2257](https://github.com/no-phux/phux/commit/87e22571c5e1a9b6196691bda8198189d07d49d7))

## [0.2.2](https://github.com/no-phux/phux/compare/pi-extension-v0.2.1...pi-extension-v0.2.2) (2026-09-02)


### Bug Fixes

* **integrations:** publish npm packages through OIDC ([#496](https://github.com/no-phux/phux/issues/496)) ([6c02514](https://github.com/no-phux/phux/commit/6c025145))

## [0.2.1](https://github.com/phall1/phux/compare/pi-extension-v0.2.0...pi-extension-v0.2.1) (2026-08-27)


### Bug Fixes

* **integrations:** size the pi process-group startup budget to real cold starts ([c1d25a1](https://github.com/phall1/phux/commit/c1d25a1da75aa9df3570f24a9ff77e6a23845cff))

## [0.2.0](https://github.com/phall1/phux/compare/pi-extension-v0.1.0...pi-extension-v0.2.0) (2026-08-14)


### ⚠ BREAKING CHANGES

* **cli:** the following deprecated spellings no longer parse. Each now fails with clap's ordinary unknown-subcommand/unknown-flag error (exit 2), not a panic:

### Features

* **agents:** append cache-preserving fleet context ([#336](https://github.com/phall1/phux/issues/336)) ([3e7807c](https://github.com/phall1/phux/commit/3e7807c3f8e45e16fe9506616e8caeaaf185548c))
* **cli:** release-candidate pass: remove deprecated spellings and add phux update ([#379](https://github.com/phall1/phux/issues/379)) ([d8fe06c](https://github.com/phall1/phux/commit/d8fe06c6fe9312f0cbf094155f3e93dac0446382))
* **integrations:** ship first-class agent plugins ([1510b59](https://github.com/phall1/phux/commit/1510b591f454377d3cf5cf67d8309aa33b76502c))
* land agent integration foundation ([#243](https://github.com/phall1/phux/issues/243)) ([44ade38](https://github.com/phall1/phux/commit/44ade3883359c0e0dd90c40ecaccdd5622b4b1ac))
* **pi:** add shared phux terminal integration ([#223](https://github.com/phall1/phux/issues/223)) ([66dc4d0](https://github.com/phall1/phux/commit/66dc4d09a1eb9d63eace45e863b8fa8360ad7f00))


### Bug Fixes

* **integrations:** declare identity only in the pi and opencode lifecycle reporters ([#383](https://github.com/phall1/phux/issues/383)) ([00d0149](https://github.com/phall1/phux/commit/00d01495ff21a9d13ab05cf91088d2e62ae8d837))
* **pi:** add spatial orchestration parity ([cb99f18](https://github.com/phall1/phux/commit/cb99f188bd8b0b4ba63c854342c00804a47420ec))
* **pi:** arbitrate global and project package copies ([#334](https://github.com/phall1/phux/issues/334)) ([d68e3a1](https://github.com/phall1/phux/commit/d68e3a1df9b6a2136d1cdb45d3dc32b19bd0cc1f))
