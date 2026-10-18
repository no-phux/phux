# herdr - compatibility alias of starter

`herdr` was renamed to `starter`. This directory exists so
configs that still `extends` `distros/herdr/herdr.toml` keep loading
after a `git pull`. `--distro herdr` also still resolves. New configs
should extend [`starter`](../starter/README.md).
