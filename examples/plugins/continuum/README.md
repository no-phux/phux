# phux Continuum Demo Plugin

This fixture composes first-party workspace archives through external plugin
actions. It does not run inside the phux server.

```sh
export XDG_CONFIG_HOME="$PWD/examples/plugins/continuum/config"

cargo run -q -p phux --bin phux -- config plugins --json
cargo run -q -p phux --bin phux -- config run com.phux.demo.continuum autosave --json
cargo run -q -p phux --bin phux -- config run com.phux.demo.continuum restore-latest --json
```

`autosave` writes `phux workspace save` output to a profile archive. `restore-latest`
passes that archive to `phux workspace restore`. The default profile is
`default`; override it with `PHUX_WORKSPACE_PROFILE`. By default archives live
under the plugin's `state/` directory; override with `PHUX_CONTINUUM_DIR` or
`PHUX_CONTINUUM_ARCHIVE`.
