//! `phux config`, `phux plugin`, and `phux theme`.

use crate::commands::config_action;
use usage::{Args, Subcommands};

/// Inspect, scaffold, and reload the config file
///
/// phux is config-driven: defaults ship in the binary and
/// your `config.toml` is a sparse overlay merged on top. These
/// subcommands never touch a running server, except `reload`,
/// which signals attached clients to re-read their config in place.
#[derive(Debug, Args)]
pub(crate) struct ConfigArgs {
    #[usage(subcommand)]
    pub(crate) action: config_action::ConfigAction,
}

/// Manage plugin manifests in the config
///
/// This is a client-local config operation: it validates
/// `phux-plugin.toml` manifests and edits `[[plugins]]` entries in the
/// user's config without contacting a running server.
#[derive(Debug, Args)]
pub(crate) struct PluginArgs {
    #[usage(subcommand)]
    pub(crate) action: PluginAction,
}

/// Install, list, and select colour themes
///
/// A theme is a directory holding an Omarchy-schema `colors.toml`.
/// Installed themes live under the phux data dir; enabled plugins may
/// contribute more. `set` edits `[theme] name` in your config and
/// signals attached clients to repaint; everything else is local.
#[derive(Debug, Args)]
pub(crate) struct ThemeArgs {
    #[usage(subcommand)]
    pub(crate) action: ThemeAction,
}

/// `phux theme <action>` — the theme catalog.
#[derive(Debug, Subcommands)]
pub(crate) enum ThemeAction {
    /// List installed and plugin-provided themes; `*` marks the selected one.
    #[usage(alias = "ls")]
    List {
        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Show a theme's resolved colours, or the selected theme's when NAME
    /// is omitted.
    ///
    /// Prints the mode, every semantic colour after Omarchy's fallback
    /// cascade, the shared palette-0..15 mapping, and the foreground-on-
    /// background contrast ratio. A client that does not read plugin
    /// manifests uses `--json` here to resolve a name.
    Show {
        /// Catalog name. Omit for the theme `config.toml` selects.
        name: Option<String>,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Select a theme: write `[theme] name` (or `file`) and signal a reload.
    ///
    /// With NAME, the catalog entry must exist and load. With `--file`, any
    /// `colors.toml` path is accepted (`~/` expands), so pointing it at
    /// `~/.local/state/omarchy/current/theme/colors.toml` follows the Omarchy
    /// theme switcher. The other key is removed so exactly one selects.
    /// Explicit `[theme]` slot keys keep overriding the selected theme.
    Set {
        /// Catalog name (see `phux theme list`).
        name: Option<String>,

        /// A `colors.toml` to follow instead of a catalog name.
        #[usage(long, value_name = "PATH")]
        file: Option<std::path::PathBuf>,
    },

    /// Install a theme from a git URL or a local directory.
    ///
    /// SOURCE is cloned with the system `git` (or copied) into the managed
    /// theme directory (`$XDG_DATA_HOME/phux/themes`, else
    /// `~/.local/share/phux/themes`) under a name derived from the URL as
    /// Omarchy derives it: the last path segment minus `.git`, an
    /// `omarchy-` or `phux-` prefix, and a `-theme` suffix. It must hold
    /// `colors.toml` at its root. Installing the same name again replaces
    /// it. Nothing in a theme is executed.
    Install {
        /// Git URL or local theme directory.
        #[usage(value_name = "SOURCE")]
        source: String,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Remove an installed theme by name.
    ///
    /// A plugin-provided theme is refused here: disable the plugin instead.
    #[usage(visible_aliases = ["rm"])]
    Remove {
        /// Catalog name.
        name: String,
    },
}

/// Fit phux into an Omarchy desktop
///
/// `setup` makes phux at home on Omarchy (<https://omarchy.org>): its
/// keys match Omarchy's tmux config (Ctrl+Space prefix, Alt+Enter splits,
/// Alt+1..9 tabs), its colours follow the Omarchy theme and repaint when
/// you switch, Super+Ctrl+Return opens it, Super+Ctrl+K lists its keys,
/// and its server starts with the desktop session. `remove` undoes all of
/// it; your own config.toml keeps winning throughout.
#[derive(Debug, Args)]
pub(crate) struct OmarchyArgs {
    #[usage(subcommand)]
    pub(crate) action: OmarchyAction,
}

/// `phux omarchy <action>` — the Omarchy desktop integration.
#[derive(Debug, Subcommands)]
pub(crate) enum OmarchyAction {
    /// Put phux in place on this Omarchy desktop. Safe to run again.
    ///
    /// Installs a distro layer with Omarchy's tmux keys and selects it (a
    /// distro you had selected stays underneath), follows Omarchy's
    /// current theme unless your config already picks one, adds a theme
    /// hook under `~/.config/omarchy/hooks/theme-set.d/`, and adds a marked
    /// block to `~/.config/hypr/bindings.lua` and `autostart.lua` that
    /// rebinds Super+Ctrl+Return and Super+Ctrl+K to phux and starts its
    /// server. Text outside the marked blocks is never touched.
    Setup {
        /// Print what would change without writing anything.
        #[usage(long)]
        dry_run: bool,
    },

    /// Show which parts of the Omarchy integration are in place.
    Status {
        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Undo `setup`: restore the previous distro and theme selection and
    /// delete the hook, the layer, and the marked Hyprland blocks.
    Remove {
        /// Print what would change without writing anything.
        #[usage(long)]
        dry_run: bool,
    },

    /// Print every key binding as `KEYS → what it does`, one per line.
    ///
    /// The input Omarchy's `omarchy-menu-select` takes, so Super+Ctrl+K
    /// can show phux's keys in Omarchy's own menu.
    Keys,
}

/// `phux plugin <action>` — local plugin registry lifecycle.
#[derive(Debug, Subcommands)]
pub(crate) enum PluginAction {
    /// List configured plugin manifests.
    #[usage(alias = "ls")]
    List {
        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Add or update a manifest entry in `config.toml`.
    Link {
        /// Path to a `phux-plugin.toml` file, or a directory containing one.
        manifest: std::path::PathBuf,

        /// Register the plugin but leave it disabled.
        #[usage(long)]
        disabled: bool,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Write a minimal plugin directory.
    ///
    /// NAME is the plugin id (`hello`, `com.example.tools`). It is also
    /// the manifest `name` and, unless `--dir` is set, the directory
    /// created in the working directory. The manifest sets `version` to
    /// `0.1.0` and `min_phux_version` to this phux, and declares one
    /// example action plus one `pane-exit` hook. The action runs
    /// `scripts/hello.sh`. A README is included. A directory that already
    /// contains files is refused and left unchanged. The result passes
    /// `phux plugin validate` and `phux plugin link`.
    New {
        /// Plugin id. Also the manifest name and the default directory.
        #[usage(value_name = "NAME")]
        name: String,

        /// Directory to write. Created when missing. Refused when it
        /// exists and is not empty.
        #[usage(long, value_name = "PATH")]
        dir: Option<std::path::PathBuf>,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Fetch, build, validate, and link a plugin package.
    ///
    /// REF is chosen in this order. An existing local directory is copied.
    /// An existing local file whose name ends in `.tar`, `.tar.gz`, or
    /// `.tgz` is extracted with the system `tar`; any other existing file
    /// is an error. A git URL (`https://…`, `git@…`, `file://…`) is cloned
    /// as given with the system `git`. `owner/repo` or `owner/repo/sub/dir`
    /// that is not an existing path is GitHub shorthand: phux clones
    /// `https://github.com/owner/repo` and, when more segments follow the
    /// repo, uses them as the plugin subdirectory. Nothing is contacted
    /// except that clone.
    ///
    /// An existing local path wins over shorthand. `./foo/bar`, `../foo`,
    /// an absolute path, a `~` path, a trailing slash, and a `.` or `..`
    /// segment are always local paths, never shorthand. When a reference
    /// matches both, the local path is chosen and the message says so.
    /// `--rev` is the branch or tag for a git URL or shorthand.
    /// `--subdir` selects the subdirectory of a git clone; passing it when
    /// the reference already names a subdirectory is an error that names
    /// both, and neither is chosen.
    ///
    /// The package lands under the managed plugins directory
    /// (`$XDG_DATA_HOME/phux/plugins`, else `~/.local/share/phux/plugins`),
    /// its manifest `[[build]]` steps for this platform run with a bounded
    /// timeout and captured output, the manifest is validated (including
    /// the `min_phux_version` gate), and the result is linked into
    /// `config.toml` like `phux plugin link`. Provenance (ref, branch,
    /// subdirectory, resolved commit) is recorded in the managed
    /// directory's `plugins.lock` so `phux plugin update` can re-fetch it.
    Install {
        /// Git URL, local path, or GitHub `owner/repo[/subdir]`.
        #[usage(value_name = "REF")]
        reference: String,

        /// Branch, tag, or full 40-character commit SHA (git sources only).
        ///
        /// A commit SHA is fetched and checked out, then pinned. A short
        /// SHA is fetched when the server can resolve it.
        #[usage(long, value_name = "REV")]
        rev: Option<String>,

        /// Subdirectory of a git clone that holds `phux-plugin.toml`.
        ///
        /// For a git URL this is the only way to name a subdirectory.
        /// `owner/repo/sub/dir` already names one; passing both is an error.
        #[usage(long, value_name = "PATH")]
        subdir: Option<String>,

        /// Install and link the plugin but leave it disabled.
        #[usage(long)]
        disabled: bool,

        /// Run remote `[[build]]` steps without a confirmation prompt.
        ///
        /// A git install whose manifest has build steps asks on a terminal
        /// and refuses otherwise. A local directory or tarball does not ask.
        #[usage(long)]
        yes: bool,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Re-fetch, rebuild, and revalidate installed plugins.
    ///
    /// Reads the managed directory's `plugins.lock`, re-fetches each
    /// recorded source (all of them, or just NAME), reruns its `[[build]]`
    /// steps, revalidates the manifest, swaps the managed copy, and
    /// records the new resolved commit. A branch update is accepted only
    /// when the new commit is the recorded one or a descendant of it. A
    /// commit pinned with `--rev` stays at that SHA. `--latest` accepts a
    /// rewritten branch or moves off a pinned commit. `config.toml` is
    /// untouched — the linked manifest path does not move.
    Update {
        /// Plugin id to update. Omit to update every installed plugin.
        name: Option<String>,

        /// Accept a rewritten history, or move off a commit pinned with `--rev`.
        #[usage(long)]
        latest: bool,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Remove a configured plugin by id.
    // `rm` / `remove` are the alias-policy spellings (ADR-0065 §5): every
    // remove-shaped registry verb answers to both, and this registry's
    // canonical name predates the policy. A code comment, not a doc comment —
    // ADR ids must not leak into `--help` (see `help_inventory`).
    #[usage(visible_aliases = ["rm", "remove"])]
    Unlink {
        /// Plugin id from its manifest.
        id: String,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Enable a configured plugin by id.
    Enable {
        /// Plugin id from its manifest.
        id: String,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Disable a configured plugin by id.
    Disable {
        /// Plugin id from its manifest.
        id: String,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Validate one manifest, or every configured manifest when omitted.
    Validate {
        /// Optional path to a `phux-plugin.toml` file or plugin directory.
        manifest: Option<std::path::PathBuf>,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Show recent plugin action and hook runs, oldest first.
    ///
    /// Every plugin action run (a TUI keybinding, `phux config run`, the MCP
    /// tool) and every server hook run appends one record to a bounded log
    /// in the state directory (`plugin-runs.jsonl`): the newest 100 runs,
    /// each with its exit status, duration, and the last 4 KiB of stdout
    /// and stderr. Failed runs show the tail of their output.
    Log {
        /// How many of the newest runs to show; 20 when omitted.
        #[usage(short = 'n', long, value_name = "NUM")]
        limit: Option<u32>,

        /// Show only runs that did not exit 0.
        #[usage(long)]
        failed: bool,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },
}
