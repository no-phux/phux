//! Config-derived TUI state: built once per attach, swapped whole on reload
//! (phux-u1tq.2).
//!
//! Before this module the driver derived the same dozen values from a
//! `phux_config::Config` in three places -- a `ConfigSeed` at attach
//! (tolerant), a `ReloadedConfig` on `reload-config` (strict), and a third
//! copy for the headless rendered snapshot -- and threaded them through
//! `SessionLoop` as loose fields. [`TuiSettings`] is that set as one value
//! with one composition path and two policies:
//!
//! * **Tolerant** ([`TuiSettings::load_tolerant`], attach time). A malformed
//!   config never blocks attach. A load or status-bar build failure degrades
//!   to a visible error line on the bar row pointing at `phux config check`;
//!   a bad keybinding disables only itself (the lenient resolver), with a
//!   status-bar diagnostic naming the chord.
//! * **Strict** ([`TuiSettings::load_strict`], reload). Any parse, layer,
//!   widget, or binding failure fails the whole reload, so the caller keeps
//!   its previous, known-good settings. Nothing is ever half-applied
//!   (docs/consumers/tui.md section 4.3).
//!
//! The asymmetry is deliberate: a reload has a known-good previous config to
//! fall back on; attach does not.
//!
//! What a reload swaps versus what it leaves alone is
//! [`TuiSettings::adopt_reload`]: keybindings, resolver, theme, chrome
//! breakpoints, status bar, plugin rows, and the which-key knobs move; the
//! sidebar geometry and the mouse-capture gate are read once at attach
//! (`[sidebar]` and `defaults.mouse` are listed as not-reloadable in the
//! same doc section). The settings page (`render::overlay::settings`) reads
//! the same fact from `phux_config::settings::Applies` so it can tell the
//! user which edits land immediately.

use std::path::Path;
use std::time::Duration;

use phux_config::keybind::{BindingDiagnostic, Resolver};
use phux_config::plugin::PluginManifest;
use phux_config::widget::WidgetError;
use phux_config::{Config, ConfigError, KeybindingsCfg, SidebarPosition};

use crate::attach::paint::SidebarEdge;
use crate::attach::plugin_actions::{self, PluginActionEntry};
use crate::attach::plugin_panes::{self, PluginPaneEntry};
use crate::render::chrome::status_bar::{Position, StatusBarPainter};
use crate::render::{ChromeBreakpoints, Theme};

/// The which-key popup knobs (`[keybindings] which-key` /
/// `which-key-delay-ms`, phux-foz.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WhichKey {
    /// Whether the popup is armed at all.
    pub enabled: bool,
    /// How long the resolver may sit at a prefix before the popup shows.
    pub delay: Duration,
}

/// `[sidebar]`, folded to what the reservation math needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarSettings {
    /// `[sidebar] enabled` -- the FIRST attach's seed for the runtime
    /// `toggle-sidebar` state, which lives on the loop, not here.
    pub enabled: bool,
    /// `[sidebar] width`, in columns.
    pub width: u16,
    /// `[sidebar] position`, folded to the reservation's edge.
    pub edge: SidebarEdge,
}

/// Everything the TUI derives from the on-disk config.
///
/// Loaded once, before any user input can reach the loop: opening a
/// discovery surface must never perform config I/O under the user's
/// fingers. The in-place reload swaps the same pieces through
/// [`Self::adopt_reload`].
pub struct TuiSettings {
    /// The plugin-merged keybindings snapshot (action-finder chords, the
    /// which-key rows, the onboarding hint). `None` only when the config
    /// failed to load at attach.
    pub keybindings: Option<KeybindingsCfg>,
    /// phux-4li.5: the keybind resolver built from that snapshot. `None`
    /// exactly when [`Self::keybindings`] is.
    pub resolver: Option<Resolver>,
    /// phux-ahv.4: single source of truth for chrome + overlay colors.
    pub theme: Theme,
    /// phux-huhi: the responsive-chrome thresholds.
    pub chrome: ChromeBreakpoints,
    /// phux-nz4.5: status-bar painter, or `None` when the config composes an
    /// empty bar (the driver then reclaims the bar row).
    pub status_bar: Option<StatusBarPainter>,
    /// phux-r82.5: palette rows + manifest `keys` merged into the prefix
    /// table.
    pub plugin_actions: Vec<PluginActionEntry>,
    /// phux-r82.7: the hostable pane entries committing `plugin-pane`.
    pub plugin_panes: Vec<PluginPaneEntry>,
    /// The which-key popup knobs.
    pub which_key: WhichKey,
    /// `[sidebar]` geometry; attach-time only.
    pub sidebar: SidebarSettings,
    /// The global `defaults.mouse` gate the `RawModeGuard` install reads;
    /// attach-time only.
    pub mouse_capture: bool,
}

impl std::fmt::Debug for TuiSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiSettings")
            .field("keybindings", &self.keybindings.is_some())
            .field("resolver", &self.resolver.is_some())
            .field("theme", &self.theme)
            .field("chrome", &self.chrome)
            .field("status_bar", &self.status_bar.is_some())
            .field("plugin_actions", &self.plugin_actions.len())
            .field("plugin_panes", &self.plugin_panes.len())
            .field("which_key", &self.which_key)
            .field("sidebar", &self.sidebar)
            .field("mouse_capture", &self.mouse_capture)
            .finish()
    }
}

impl TuiSettings {
    /// Attach-time load: the layered config from its canonical path, with
    /// every failure degraded so a broken config never blocks attach.
    ///
    /// When the file does not parse at all there is no keybinding snapshot
    /// and no resolver -- the user still gets a working pane mirror, and the
    /// status bar carries the parse error pointing at `phux config check`.
    #[must_use]
    pub fn load_tolerant() -> Self {
        match phux_config::loader::load() {
            Ok(cfg) => Self::tolerant_from(&cfg),
            Err(err) => Self::without_config(&err),
        }
    }

    /// Reload: re-read the layered config at `path` and rebuild every piece
    /// of TUI state from it, strictly.
    ///
    /// This is the same loader attach runs, so `extends` stacks, `-append`
    /// array merges, and the embedded defaults all apply identically.
    ///
    /// # Errors
    ///
    /// A human-readable, single-problem message suitable for a toast --
    /// unreadable file, malformed TOML, a broken layer stack, a widget the
    /// status bar cannot build, a keybinding table the resolver rejects --
    /// never a partial result.
    pub fn load_strict(path: &Path) -> Result<Self, String> {
        let cfg = phux_config::loader::load_from(path).map_err(|err| err.to_string())?;
        Self::strict_from(&cfg)
    }

    /// Re-read `path` and swap the reloadable subset in, or leave `self`
    /// untouched and hand back the error.
    ///
    /// This is the "keep the old config, never half-apply" contract in one
    /// place: the swap happens only after [`Self::load_strict`] returned a
    /// fully built value.
    ///
    /// # Errors
    ///
    /// The [`Self::load_strict`] message; `self` is untouched.
    pub fn reload_in_place(&mut self, path: &Path) -> Result<(), String> {
        let new = Self::load_strict(path)?;
        self.adopt_reload(new);
        Ok(())
    }

    /// Take the reloadable subset from `new`.
    ///
    /// Sidebar geometry and the mouse-capture gate stay as they were: both
    /// were applied to the outer terminal and the layout at attach and are
    /// documented as taking effect on the next attach.
    pub fn adopt_reload(&mut self, new: Self) {
        self.keybindings = new.keybindings;
        self.resolver = new.resolver;
        self.theme = new.theme;
        self.chrome = new.chrome;
        self.status_bar = new.status_bar;
        self.plugin_actions = new.plugin_actions;
        self.plugin_panes = new.plugin_panes;
        self.which_key = new.which_key;
    }

    /// The seed used when the config file itself does not load: no
    /// bindings, default colors and thresholds, the sidebar off, mouse
    /// capture on, and the parse error on the bar row.
    fn without_config(err: &ConfigError) -> Self {
        tracing::warn!(error = %err, "phux-config load failed; surfacing on status bar");
        Self {
            keybindings: None,
            resolver: None,
            theme: Theme::default(),
            chrome: ChromeBreakpoints::default(),
            status_bar: Some(StatusBarPainter::error_line(config_error_line(err))),
            plugin_actions: Vec::new(),
            plugin_panes: Vec::new(),
            which_key: WhichKey {
                enabled: false,
                delay: Duration::from_millis(600),
            },
            sidebar: SidebarSettings {
                enabled: false,
                width: 0,
                edge: SidebarEdge::Left,
            },
            mouse_capture: true,
        }
    }

    /// Build tolerantly from a parsed config.
    ///
    /// The status bar degrades a build failure to the error-line painter; the
    /// resolver is the lenient one, and its diagnostics take the bar row
    /// unless a config error already owns it (which subsumes any keybinding
    /// problem).
    #[must_use]
    pub fn tolerant_from(cfg: &Config) -> Self {
        let manifests = enabled_manifests(cfg);
        let mut status_bar = match compose_status_bar(cfg, &manifests) {
            Ok(painter) => painter,
            Err(err) => {
                tracing::warn!(error = %err, "status-bar build failed; surfacing on status bar");
                Some(StatusBarPainter::error_line(config_error_line(&err)))
            }
        };
        let plugin_actions = plugin_actions::entries_from_manifests(&manifests);
        let plugin_panes = plugin_panes::entries_from_manifests(&manifests);
        let keybindings = merged_keybindings(cfg, &plugin_actions);
        let (resolver, diagnostics) = build_resolver_from(&keybindings);
        if !diagnostics.is_empty()
            && !status_bar
                .as_ref()
                .is_some_and(StatusBarPainter::is_error_line)
        {
            status_bar = Some(StatusBarPainter::error_line(keybind_error_line(
                &diagnostics,
            )));
        }
        Self::assemble(
            cfg,
            keybindings,
            resolver,
            status_bar,
            plugin_actions,
            plugin_panes,
        )
    }

    /// Build strictly from a parsed config: any widget or binding the
    /// composition rejects fails the whole build.
    ///
    /// # Errors
    ///
    /// The widget or keybinding error, rendered for a toast.
    pub fn strict_from(cfg: &Config) -> Result<Self, String> {
        let manifests = enabled_manifests(cfg);
        // Widget composition is the one post-parse validation step that can
        // still reject the config.
        let status_bar = compose_status_bar(cfg, &manifests).map_err(|err| err.to_string())?;
        let plugin_actions = plugin_actions::entries_from_manifests(&manifests);
        let plugin_panes = plugin_panes::entries_from_manifests(&manifests);
        let keybindings = merged_keybindings(cfg, &plugin_actions);
        // phux-i0e8.3.4: deliberately the STRICT build. Reload keeps its
        // all-or-nothing contract: any binding the resolver rejects fails the
        // whole reload and the previous config stays fully in effect.
        let resolver = Resolver::new(&keybindings).map_err(|err| err.to_string())?;
        Ok(Self::assemble(
            cfg,
            keybindings,
            resolver,
            status_bar,
            plugin_actions,
            plugin_panes,
        ))
    }

    /// The policy-independent tail of both builds.
    fn assemble(
        cfg: &Config,
        keybindings: KeybindingsCfg,
        resolver: Resolver,
        mut status_bar: Option<StatusBarPainter>,
        plugin_actions: Vec<PluginActionEntry>,
        plugin_panes: Vec<PluginPaneEntry>,
    ) -> Self {
        let theme = Theme::from_cfg(&cfg.theme);
        // phux-foz.1: the attention hint's chip color comes from the theme's
        // `attention` slot rather than a hardcoded SGR in the painter.
        if let Some(sb) = status_bar.as_mut() {
            sb.set_attention_color(theme.attention);
        }
        Self {
            which_key: WhichKey {
                enabled: keybindings.which_key,
                delay: Duration::from_millis(keybindings.which_key_delay_ms),
            },
            keybindings: Some(keybindings),
            resolver: Some(resolver),
            theme,
            chrome: ChromeBreakpoints::from_cfg(&cfg.chrome),
            status_bar,
            plugin_actions,
            plugin_panes,
            sidebar: SidebarSettings {
                enabled: cfg.sidebar.enabled,
                width: cfg.sidebar.width,
                edge: sidebar_edge(cfg.sidebar.position),
            },
            mouse_capture: cfg.defaults.mouse,
        }
    }

    /// Which outer-terminal row the bar reserves, or `None` for no bar.
    #[must_use]
    pub fn bar_position(&self) -> Option<Position> {
        self.status_bar.as_ref().map(StatusBarPainter::position)
    }

    /// The `exec` widget feeds the driver spawns bounded interval runners
    /// for.
    #[must_use]
    pub fn exec_feeds(&self) -> Vec<phux_config::widget::ExecFeed> {
        self.status_bar
            .as_ref()
            .map(StatusBarPainter::exec_feeds)
            .unwrap_or_default()
    }
}

/// `[sidebar] position`, folded to the reservation's edge.
#[must_use]
pub const fn sidebar_edge(position: SidebarPosition) -> SidebarEdge {
    match position {
        SidebarPosition::Right => SidebarEdge::Right,
        SidebarPosition::Left => SidebarEdge::Left,
    }
}

/// phux-r82.5 / phux-r82.7: the enabled plugins' manifests, resolved
/// relative to the canonical config path -- the same resolution
/// `phux config run` uses. A broken manifest is skipped with a warning.
fn enabled_manifests(cfg: &Config) -> Vec<PluginManifest> {
    if cfg.plugins.is_empty() {
        return Vec::new();
    }
    phux_config::plugin::load_enabled_manifests(&phux_config::loader::config_path(), &cfg.plugins)
}

/// The keybindings snapshot with the plugin manifests' `keys` merged in
/// (user config wins every conflict), cached so opening a discovery surface
/// never performs config I/O under user fingers.
fn merged_keybindings(cfg: &Config, plugin_actions: &[PluginActionEntry]) -> KeybindingsCfg {
    let mut kb = cfg.keybindings.clone();
    plugin_actions::merge_plugin_bindings(&mut kb, plugin_actions);
    kb
}

/// Compose the status-bar painter from a parsed [`Config`] plus the
/// enabled plugins' manifests.
///
/// This is the ONE composition point shared by the tolerant and strict
/// builds, so the two cannot drift (phux-i0e8.6.1: reload used to hardcode
/// the default position and skip
/// [`phux_config::widget::merge_widget_contributions`], silently resetting
/// `[status] position = "top"` and dropping plugin-contributed widgets).
/// Plugin `[[widgets]]` contributions merge in **after** the user's own
/// `[status]` widgets and **before** the bar builds, and the painter takes
/// `cfg.status.position` and the configured prefix. `Ok(None)` means the
/// merged config composes an empty bar.
///
/// Only the composition is shared; error POLICY stays with the caller.
///
/// # Errors
///
/// Forwards the [`phux_config::widget::StatusBar::build`] error (unknown
/// widget kind, bad option) untouched for the caller's policy.
pub fn compose_status_bar(
    cfg: &Config,
    manifests: &[PluginManifest],
) -> Result<Option<StatusBarPainter>, WidgetError> {
    let registry = phux_config::WidgetRegistry::with_builtins();
    // phux-r82.6: fold enabled plugins' `[[widgets]]` contributions in
    // after the user's own `[status]` widgets. Invalid contributions are
    // dropped with a warning inside the merge (mirroring the plugin
    // keybinding policy), so a broken plugin cannot fail the build; a
    // genuinely broken USER config still can.
    let mut status = cfg.status.clone();
    phux_config::widget::merge_widget_contributions(&mut status, manifests, &registry);
    let bar = phux_config::widget::StatusBar::build(&status, &registry)?;
    if bar.is_empty() {
        return Ok(None);
    }
    // phux-foz.8: `[status] position = "top" | "bottom"` picks the
    // reserved row; the pane content rect shifts to match (see
    // `paint::content_rect`).
    let mut painter = StatusBarPainter::new(bar, cfg.status.position.into());
    painter.set_prefix(cfg.keybindings.prefix.clone());
    Ok(Some(painter))
}

/// Build the lenient [`Resolver`] from a keybindings snapshot (phux-4li.5).
///
/// The snapshot is the plugin-merged one (phux-r82.5), so manifest `keys`
/// chords resolve like user bindings -- the merge already validated each
/// contributed chord, so a plugin can't poison this build.
///
/// phux-i0e8.3.4: the build is **lenient per binding** -- a resolver always
/// comes back, and each diagnostic disables exactly the binding it names.
/// Before this, one malformed chord failed the whole build and silently
/// disabled EVERY binding, including `detach`. Diagnostics are logged here;
/// the caller surfaces them as a visible status-bar error line
/// ([`keybind_error_line`]). Config reload deliberately stays
/// all-or-nothing instead ([`TuiSettings::strict_from`]).
#[must_use]
pub fn build_resolver_from(kb: &KeybindingsCfg) -> (Resolver, Vec<BindingDiagnostic>) {
    let (resolver, diagnostics) = Resolver::new_lenient(kb);
    for diag in &diagnostics {
        tracing::warn!(binding = %diag.binding, error = %diag.error, "keybinding disabled");
    }
    (resolver, diagnostics)
}

/// Format the lenient resolver's diagnostics as the one-line status-bar
/// error strip (phux-i0e8.3.4).
///
/// Names the first offending chord, the reason, how many more bindings (if
/// any) were also disabled, and the actionable next step (`phux config
/// check`). Empty input formats to an empty string (callers gate on
/// non-empty diagnostics).
#[must_use]
pub fn keybind_error_line(diags: &[BindingDiagnostic]) -> String {
    let Some(first) = diags.first() else {
        return String::new();
    };
    let more = diags.len() - 1;
    if more == 0 {
        format!(
            "keybinding \"{}\" disabled: {} (run: phux config check)",
            first.binding, first.error
        )
    } else {
        format!(
            "keybinding \"{}\" disabled: {} (+{more} more; run: phux config check)",
            first.binding, first.error
        )
    }
}

/// Format a one-line, on-screen config error for the status bar (phux-9vf).
///
/// The `Display` of the error plus the actionable next step. The remedy is
/// `phux config check` -- the verb that diagnoses, with key paths and layer
/// attribution -- not `config show`, which only renders the effective config
/// (phux-i0e8.3.5).
pub fn config_error_line(err: &impl std::fmt::Display) -> String {
    format!("config error: {err} (run: phux config check)")
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_config::keybind::{Feed, parse_chord};

    /// Write `contents` as a config file inside a fresh temp dir and
    /// return `(dir_guard, config_path)`.
    fn config_file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, contents).expect("write config");
        (dir, path)
    }

    fn parse(toml: &str) -> Config {
        phux_config::parse_with_defaults(toml, Path::new("test.toml")).expect("config parses")
    }

    #[test]
    fn strict_load_applies_keybinding_and_theme_changes() {
        let (_dir, path) = config_file(
            r##"
            [keybindings.prefix-table]
            X = "kill-pane"

            [theme]
            accent = "#ff0000"
            "##,
        );
        let new = TuiSettings::load_strict(&path).expect("valid config reloads");
        let kb = new.keybindings.expect("snapshot present");
        assert!(
            kb.prefix_table.contains_key("X"),
            "reload must pick up the new prefix-table binding",
        );
        assert_eq!(
            new.theme.accent,
            ratatui::style::Color::Rgb(0xff, 0, 0),
            "reload must pick up the new theme accent",
        );
        // The resolver is built from the same snapshot: the new chord
        // resolves (prefix, then X) to the bound action.
        let mut resolver = new.resolver.expect("resolver present");
        let prefix = parse_chord(&kb.prefix).expect("prefix parses");
        assert_eq!(resolver.feed(prefix), Feed::Partial);
        match resolver.feed(parse_chord("X").expect("chord parses")) {
            Feed::Resolved(ra) => assert_eq!(ra.action, "kill-pane"),
            other => panic!("expected the reloaded binding to resolve, got {other:?}"),
        }
    }

    #[test]
    fn strict_load_reads_which_key_and_sidebar_knobs() {
        let (_dir, path) = config_file(
            r#"
            [keybindings]
            which-key = false
            which-key-delay-ms = 250

            [sidebar]
            enabled = true
            width = 33
            position = "right"

            [defaults]
            mouse = false
            "#,
        );
        let new = TuiSettings::load_strict(&path).expect("valid config reloads");
        assert!(!new.which_key.enabled);
        assert_eq!(new.which_key.delay, Duration::from_millis(250));
        assert_eq!(
            new.sidebar,
            SidebarSettings {
                enabled: true,
                width: 33,
                edge: SidebarEdge::Right,
            }
        );
        assert!(!new.mouse_capture);
    }

    #[test]
    fn failed_reload_keeps_previous_settings() {
        // Seed "previous" state clearly distinguishable from defaults.
        let mut settings = TuiSettings::tolerant_from(&parse(
            r"
            [keybindings]
            which-key-delay-ms = 123
            [sidebar]
            width = 41
            ",
        ));
        settings.theme = Theme {
            accent: ratatui::style::Color::Rgb(1, 2, 3),
            ..Theme::default()
        };
        settings.chrome = ChromeBreakpoints {
            compact_cols: 99,
            ..ChromeBreakpoints::DEFAULT
        };
        let old_theme = settings.theme;
        let old_chrome = settings.chrome;

        let (_dir, path) = config_file("this is [not valid toml");
        let err = settings
            .reload_in_place(&path)
            .expect_err("malformed TOML must fail the reload");
        assert!(!err.is_empty(), "error message must be surfaceable");

        // Every slot is exactly as it was: nothing half-applied.
        assert!(settings.keybindings.is_some());
        assert!(settings.resolver.is_some());
        assert_eq!(settings.theme, old_theme);
        assert_eq!(
            settings.chrome, old_chrome,
            "breakpoints must not be half-applied"
        );
        assert_eq!(settings.which_key.delay, Duration::from_millis(123));
        assert_eq!(settings.sidebar.width, 41);
    }

    #[test]
    fn strict_build_rejects_a_bad_binding_that_the_tolerant_build_degrades() {
        let cfg = parse(
            r#"
            [keybindings.prefix-table]
            "q-" = "kill-pane"
            d = "detach"
            "#,
        );
        let err = TuiSettings::strict_from(&cfg).expect_err("strict rejects the malformed chord");
        assert!(!err.is_empty());

        let tolerant = TuiSettings::tolerant_from(&cfg);
        // The lenient resolver survives, and the bar row names the chord.
        let mut resolver = tolerant.resolver.expect("lenient resolver present");
        let prefix = parse_chord("C-a").expect("prefix parses");
        assert_eq!(resolver.feed(prefix), Feed::Partial);
        assert!(matches!(
            resolver.feed(parse_chord("d").expect("chord parses")),
            Feed::Resolved(_)
        ));
        let bar = tolerant.status_bar.expect("error-line painter present");
        assert!(bar.is_error_line(), "the diagnostic takes the bar row");
    }

    #[test]
    fn adopt_reload_leaves_attach_time_settings_alone() {
        let mut settings = TuiSettings::tolerant_from(&parse(
            r"
            [sidebar]
            width = 41
            [defaults]
            mouse = false
            ",
        ));
        let new = TuiSettings::strict_from(&parse(
            r##"
            [sidebar]
            width = 12
            [defaults]
            mouse = true
            [theme]
            accent = "#010203"
            "##,
        ))
        .expect("valid");
        settings.adopt_reload(new);
        assert_eq!(settings.theme.accent, ratatui::style::Color::Rgb(1, 2, 3));
        assert_eq!(
            settings.sidebar.width, 41,
            "sidebar geometry is attach-time"
        );
        assert!(!settings.mouse_capture, "mouse capture is attach-time");
    }

    #[test]
    fn without_config_carries_the_error_on_the_bar_row() {
        let settings = TuiSettings::without_config(&ConfigError::Parse {
            path: "x.toml".into(),
            position: None,
            message: "boom".to_owned(),
        });
        assert!(settings.keybindings.is_none());
        assert!(settings.resolver.is_none());
        let bar = settings.status_bar.expect("error line present");
        assert!(bar.is_error_line());
        assert!(!settings.sidebar.enabled);
        assert!(settings.mouse_capture);
    }
}
