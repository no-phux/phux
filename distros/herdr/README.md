# herdr

This directory is a phux compatibility layer aliasing the `starter`
config. It is not herdr; herdr is a separate, independent project.

phux ships the alias for people who know herdr's keys and workflow.
`herdr.toml` extends [`starter`](../starter/README.md) and adds no
settings of its own. Configs that still `extends`
`distros/herdr/herdr.toml` keep loading after a `git pull`. `--distro
herdr` also still resolves. New configs should extend
[`starter`](../starter/README.md).
