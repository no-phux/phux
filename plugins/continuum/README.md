# Continuum

Saves and restores workspace archives through external plugin actions. It
does not run inside the phux server or preserve running processes.

```sh
export XDG_CONFIG_HOME="$PWD/plugins/continuum/config"

cargo run -q -p phux --bin phux -- config plugins --json
cargo run -q -p phux --bin phux -- config run com.phux.demo.continuum autosave --json
cargo run -q -p phux --bin phux -- config run com.phux.demo.continuum restore-latest --json
```

`autosave` writes `phux archive export` output to a profile archive. `restore-latest`
passes that archive to `phux archive import`. The default profile is
`default`; override it with `PHUX_WORKSPACE_PROFILE`. By default archives live
under the plugin's `state/` directory; override with `PHUX_CONTINUUM_DIR` or
`PHUX_CONTINUUM_ARCHIVE`.

Existing checkout users: [preserve old plugin state](../README.md#moving-from-examplesplugins)
before using the relocated package.
