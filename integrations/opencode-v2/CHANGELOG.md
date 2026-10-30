# Changelog

## [0.8.0](https://github.com/no-phux/phux/compare/opencode-plugin-v0.7.1...opencode-plugin-v0.8.0) (2026-10-30)


### Features

* **opencode:** phux panes, live pane views, and agent attention in OpenCode Desktop ([#1850](https://github.com/no-phux/phux/issues/1850)) ([cd7b785](https://github.com/no-phux/phux/commit/cd7b78520203473b1a6e4529c0fa27f71350753d))

## [0.7.1](https://github.com/no-phux/phux/compare/opencode-plugin-v0.7.0...opencode-plugin-v0.7.1) (2026-10-27)


### Documentation

* use one canonical one-liner and fix protocol and windows claims ([#1797](https://github.com/no-phux/phux/issues/1797)) ([1855dda](https://github.com/no-phux/phux/commit/1855dda8c0dc4a5dd293f7cde525f2f2a60bf97b))

## [0.7.0](https://github.com/no-phux/phux/compare/opencode-plugin-v0.6.2...opencode-plugin-v0.7.0) (2026-10-22)


### Features

* **agents:** mail reaches the agent's conversation, never its terminal ([#1721](https://github.com/no-phux/phux/issues/1721)) ([b25f28e](https://github.com/no-phux/phux/commit/b25f28e02324636311892f2a16efffd4230ddd0f))

## [0.6.2](https://github.com/no-phux/phux/compare/opencode-plugin-v0.6.1...opencode-plugin-v0.6.2) (2026-10-18)

### Bug Fixes

* **opencode:** refresh Bun types and pin the paired Seroval packages to 1.6.9 while preserving OpenCode 2.0.32 and its native UI runtime versions.

## [0.6.1](https://github.com/no-phux/phux/compare/opencode-plugin-v0.6.0...opencode-plugin-v0.6.1) (2026-10-18)


### Bug Fixes

* **opencode:** align stable SDK and compile terminal companion ([#1606](https://github.com/no-phux/phux/issues/1606)) ([b0187d6](https://github.com/no-phux/phux/commit/b0187d61c3994bdb6779e8e2cc20fddc97d2109a))

## [0.6.0](https://github.com/no-phux/phux/compare/opencode-plugin-v0.5.0...opencode-plugin-v0.6.0) (2026-10-16)


### Features

* **agent:** support ACP-owned sessions across harnesses ([#1431](https://github.com/no-phux/phux/issues/1431)) ([2696d16](https://github.com/no-phux/phux/commit/2696d16a273f58da0d100dda61816f6a8d50f314))

## [0.5.0](https://github.com/no-phux/phux/compare/opencode-plugin-v0.4.0...opencode-plugin-v0.5.0) (2026-10-15)


### Features

* **opencode:** show owned terminals in sidebar ([aaacf2e](https://github.com/no-phux/phux/commit/aaacf2eb2cc590b335e2d12e06ea9c58721fab3a))

## [0.4.0](https://github.com/no-phux/phux/compare/opencode-plugin-v0.3.0...opencode-plugin-v0.4.0) (2026-10-14)


### Features

* **agent-rules:** ship verified gemini-cli, goose, and aider manifests ([5a3ed51](https://github.com/no-phux/phux/commit/5a3ed51a71a9266a36224acc00d0c9c1c7e734e5))
* **agent:** native agent transcripts over provider_raw (phux.transcript/v1, ADR-0156) ([#1106](https://github.com/no-phux/phux/issues/1106)) ([5b1c728](https://github.com/no-phux/phux/commit/5b1c728357aae676d222a96c94acfb6294d101eb))
* **integrations:** complete native terminal controls for OMP OpenCode and Pi ([7ee9fa3](https://github.com/no-phux/phux/commit/7ee9fa37f8acb3f672f10fac8dcb08d92b9341b5))
* **integrations:** publish the OpenCode, OMP and Grok integrations ([#1228](https://github.com/no-phux/phux/issues/1228)) ([c259672](https://github.com/no-phux/phux/commit/c2596722c7a6cf7ef6cedcea9632d0925b14e049))
* **opencode:** report active phux pane from V2 terminal client ([27210f1](https://github.com/no-phux/phux/commit/27210f17a3f17af137caca572208a8dad8ddd75c))


### Bug Fixes

* **agent:** classify all captured first-party agent kinds ([661af18](https://github.com/no-phux/phux/commit/661af180aad58673376a2d9de057f03a2a91937f))
* **integrations:** accept canonical v3 session inventory ([fe125b5](https://github.com/no-phux/phux/commit/fe125b5eef8b9ceeac090e1440573d7a6aa2e5db))
* **integrations:** align plugins with current harness APIs ([ddee98e](https://github.com/no-phux/phux/commit/ddee98e339e9b76fe6a2a5c62450813ff0cb4cfb))
* **integrations:** fence session adoption and support RPC selection ([6b3c3c1](https://github.com/no-phux/phux/commit/6b3c3c127ee8e6e40cb819a6bbe8093b7d4843fd))
* **integrations:** resend an agent emit the upgrade seal refused ([#1032](https://github.com/no-phux/phux/issues/1032)) ([9540d76](https://github.com/no-phux/phux/commit/9540d76c4beb8ad60366fc9c2c774da8e8fc0f95))
* **integrations:** restore Pi fleet context and hosting identity ([9f8270e](https://github.com/no-phux/phux/commit/9f8270e0b6bfc3ade63cb3e229f32952b7fa5066))
* **integrations:** stop the OpenCode plugin from hijacking the shell ([c63eec9](https://github.com/no-phux/phux/commit/c63eec9d0c155d3c87822de9dd297c782e93c048))
* **opencode:** avoid duplicate phux plugins in worktrees ([#1175](https://github.com/no-phux/phux/issues/1175)) ([d87474d](https://github.com/no-phux/phux/commit/d87474d3c3e8041644ccadeb9ac71f7fb320d18f))
* **opencode:** run the built-in shell on the selected sibling pane ([#1125](https://github.com/no-phux/phux/issues/1125)) ([4f63908](https://github.com/no-phux/phux/commit/4f639082a91ee39b34a7ecb4b9d93046022cbfa5))


### Documentation

* **agent:** describe per-pane acknowledged input admission ([93c0734](https://github.com/no-phux/phux/commit/93c0734a18a14dcd1f2fcc4c3417ad84d75d6b00))
