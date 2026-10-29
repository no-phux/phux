# herdr (alias of starter)

`herdr` is the former name of phux's `starter` config layer, kept as an
alias so existing configs keep loading. It is not [Herdr](https://herdr.dev),
a separate project.

`herdr.toml` extends [`starter`](../starter/README.md) and adds no
settings of its own. Configs that still `extends`
`distros/herdr/herdr.toml` keep loading after a `git pull`. `--distro
herdr` also still resolves. New configs should extend
[`starter`](../starter/README.md).
