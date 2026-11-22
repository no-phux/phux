# Theme plugin

A minimal working plugin: [phux-plugin.toml](./phux-plugin.toml) declares the
theme in [themes/phux-dark/colors.toml](./themes/phux-dark/colors.toml).
There are no scripts or background processes.

From a phux checkout, validate it before installing:

```sh
phux plugin validate examples/plugins/theme-pack
phux plugin install examples/plugins/theme-pack
```

Then select `phux-dark` in your [theme settings](../../../docs/themes.md).
See [plugins](../../../docs/plugins.md) for manifest fields and removal.
