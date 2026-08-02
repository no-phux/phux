use clap::Subcommand;

/// `phux config <action>` — local config inspection and scaffolding.
#[derive(Debug, Subcommand)]
pub(crate) enum ConfigAction {
    /// Write a commented starter config to the canonical path.
    ///
    /// The file is the shipped defaults, fully commented out: inert until
    /// you uncomment a line, so the binary's defaults stay authoritative.
    /// Refuses to overwrite an existing config unless `--force`.
    ///
    /// With `--distro`, the scaffold additionally carries one active
    /// `extends` line layering the named starter distribution (a bundled
    /// name like `herdr`, or a path to a distro layer `.toml`) between
    /// the shipped defaults and your file.
    Init {
        /// Overwrite an existing config file instead of refusing.
        #[arg(long)]
        force: bool,

        /// Starter distribution to extend: a bundled name (resolved
        /// under `$PHUX_DISTROS_DIR`, the XDG data dir, or the repo
        /// checkout) or a path to a distro layer `.toml` / directory.
        #[arg(long, value_name = "NAME_OR_PATH")]
        distro: Option<String>,
    },

    /// Print the resolved config path. Pure path math — prints the path
    /// whether or not the file exists.
    Path,

    /// Validate the config and report every problem, with full key paths.
    ///
    /// The loader already refuses an unknown key, but it names only the leaf
    /// field (`unknown field 'enabledd'`) and stops at the first one. This
    /// reports `sidebar.enabledd`, names the layer file that introduced it,
    /// and finds every problem in one pass — so a config with four typos
    /// takes one edit, not four.
    ///
    /// Exits 0 when clean and 1 when anything was found, so it can gate a
    /// dotfiles CI run.
    Check {
        /// Config file to check. Defaults to the resolved config path.
        #[arg(value_name = "PATH")]
        path: Option<std::path::PathBuf>,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Print the effective config (shipped defaults + your overrides) as
    /// TOML. With `--default`, print the shipped defaults verbatim
    /// instead, ignoring any user config. With `--layers`, print which
    /// layer of the `extends` stack set each effective key instead of
    /// the values.
    Show {
        /// Show the shipped defaults verbatim, not the merged result.
        #[arg(long, conflicts_with_all = ["layers", "json"])]
        default: bool,

        /// Attribute each effective key to the layer that set it
        /// (embedded defaults / `extends` layers / your config file).
        #[arg(long)]
        layers: bool,

        /// With --layers: emit a stable JSON document instead of human
        /// text.
        #[arg(long, requires = "layers")]
        json: bool,
    },

    /// List plugin manifests declared by `[[plugins]]`.
    Plugins {
        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// List agent states from configured plugin manifests, merged with
    /// live `phux.agent/v1` records when a server is running.
    Agents {
        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Re-read the layered config and apply it to running clients in
    /// place.
    ///
    /// Validates the config locally first (a broken file fails here, with
    /// the parse error, and nothing is signalled), then rings the
    /// `phux.config.reload/v1` doorbell on the server so every attached
    /// client re-reads its own config file and rebuilds keybindings,
    /// theme, and status bar without restarting. Clients whose re-read
    /// fails keep their previous config. Deliberately explicit — the
    /// config file is never watched (see docs/consumers/tui.md section
    /// 4.3).
    Reload,

    /// Execute one action declared by a configured plugin manifest.
    Run {
        /// Configured plugin id.
        plugin: String,

        /// Plugin-local action id.
        action: String,

        /// Give up after this many seconds. Omit to wait indefinitely.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,

        /// Override the action cwd. Relative paths resolve under plugin root.
        #[arg(long, value_name = "PATH")]
        cwd: Option<std::path::PathBuf>,

        /// Emit the structured action result as JSON.
        #[arg(long)]
        json: bool,
    },
}
