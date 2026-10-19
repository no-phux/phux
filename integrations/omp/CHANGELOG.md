# Changelog

## Unreleased

### Features

* Stream bounded native conversations from terminal-started OMP, including replies,
  visible thinking and tool progress, through the existing AgentSession transport.
* Replay the selected branch on startup and replace the stream on idle tree
  navigation so native clients do not retain abandoned replies.
* Honor `PHUX_AGENT_TRANSCRIPT=0` and opt-in `full` tool output using the shared
  Pi/OMP transcript mapper.

## [0.3.1](https://github.com/no-phux/phux/compare/omp-extension-v0.3.0...omp-extension-v0.3.1) (2026-10-19)


### Documentation

* conserve live contracts and make historical decisions searchable ([#1643](https://github.com/no-phux/phux/issues/1643)) ([bb4e5da](https://github.com/no-phux/phux/commit/bb4e5daf69896fded96136a954377513b387e5da))

## [0.3.0](https://github.com/no-phux/phux/compare/omp-extension-v0.2.0...omp-extension-v0.3.0) (2026-10-16)


### Features

* **agent:** support ACP-owned sessions across harnesses ([#1431](https://github.com/no-phux/phux/issues/1431)) ([2696d16](https://github.com/no-phux/phux/commit/2696d16a273f58da0d100dda61816f6a8d50f314))

## [0.2.0](https://github.com/no-phux/phux/compare/omp-extension-v0.1.0...omp-extension-v0.2.0) (2026-10-15)


### Features

* **native:** read terminal omp conversations in gpui ([#1375](https://github.com/no-phux/phux/issues/1375)) ([2d267e7](https://github.com/no-phux/phux/commit/2d267e7a9ac331e91dab44afde9d7dddee64e0d7))
## 0.1.0 (2026-10-14)


### Features

* **integrations:** complete native terminal controls for OMP OpenCode and Pi ([7ee9fa3](https://github.com/no-phux/phux/commit/7ee9fa37f8acb3f672f10fac8dcb08d92b9341b5))
* **integrations:** publish the OpenCode, OMP and Grok integrations ([#1228](https://github.com/no-phux/phux/issues/1228)) ([c259672](https://github.com/no-phux/phux/commit/c2596722c7a6cf7ef6cedcea9632d0925b14e049))
* **omp:** report host-bound native session lifecycle ([2a261d2](https://github.com/no-phux/phux/commit/2a261d2c35c86ae38ebcd1c529c24c3eebf3cb74))


### Bug Fixes

* **integrations:** align plugins with current harness APIs ([ddee98e](https://github.com/no-phux/phux/commit/ddee98e339e9b76fe6a2a5c62450813ff0cb4cfb))
* **omp:** fence lifecycle causality and preserve approval state ([b53bdca](https://github.com/no-phux/phux/commit/b53bdca8d3e6cd825d83c9c12589920d73df354e))
* **omp:** initialize cold unbound native identities safely ([add1a5c](https://github.com/no-phux/phux/commit/add1a5ce918bd01e24e3f40342803bef4b1f76b6))
* **omp:** keep reporting when input is queued into a busy run ([b718618](https://github.com/no-phux/phux/commit/b71861826a51526e75fd3d5d2846391db37c3d67))
* **omp:** pin the post-end tool delivery the lifecycle smoke assumed away ([c420325](https://github.com/no-phux/phux/commit/c420325c806178b688a71350f271ed8e3531168d))
