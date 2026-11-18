# Continuum

Saves workspace archives through server event hooks and restores them via
the CLI. It does not run inside the phux server or preserve running processes.

```sh
export XDG_CONFIG_HOME="$PWD/plugins/continuum/config"

cargo run -q -p phux --bin phux -- config plugins --json
phux --socket "$SOCKET" archive export --output "$ARCHIVE"
phux --socket "$SOCKET" archive import "$ARCHIVE"
```

The `autosave` hook writes `phux archive export` output to a profile archive.
The default profile is
`default`; override it with `PHUX_WORKSPACE_PROFILE`. By default archives live
under the plugin's `state/` directory; override with `PHUX_CONTINUUM_DIR` or
`PHUX_CONTINUUM_ARCHIVE`.

Existing checkout users: [preserve old plugin state](../README.md#moving-from-examplesplugins)
before using the relocated package.
