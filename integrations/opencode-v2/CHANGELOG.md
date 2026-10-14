# Changelog

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
