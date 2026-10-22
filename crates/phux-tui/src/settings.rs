//! Config-derived TUI state: built once per attach, swapped whole on reload.
//!
//! [`TuiSettings`] has one composition path and two policies:
//!
//! * **Tolerant** ([`TuiSettings::load_tolerant`], attach). A malformed
//!   config never blocks attach: a file that does not load falls back to
//!   the shipped defaults, failures degrade to an error line on the bar
//!   pointing at `phux config check`, and a bad keybinding disables only
//!   itself.
//! * **Strict** ([`TuiSettings::load_strict`], reload). Any failure fails
//!   the whole reload and the previous settings stay; nothing is half-applied
//!   (docs/config.md#when-a-change-takes-effect).
//!
//! [`TuiSettings::adopt_reload`] swaps keybindings, resolver, theme, chrome
//! breakpoints, status bar, plugin rows and sidebar sections, and which-key
//! knobs; sidebar geometry and the mouse gate are attach-time only.

use std::path::Path;
use std::time::Duration;

use phux_config::keybind::{BindingDiagnostic, Resolver};
use phux_config::plugin::PluginManifest;
use phux_config::widget::WidgetError;
use phux_config::{Config, ConfigError, KeybindingsCfg, SidebarPosition};

use crate::attach::paint::SidebarEdge;
use crate::attach::plugin_actions::{self, PluginActionEntry};
use crate::attach::plugin_panes::{self, PluginPaneEntry};
use crate::attach::plugin_sidebar;
use crate::render::chrome::sidebar_sections::PluginSectionSpec;
use crate::render::chrome::status_bar::StatusBarPainter;
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
    /// `[sidebar] agents-share`: percent of the body the Agents panel keeps
    /// whatever its population; `0` drops it.
    pub agents_share: u8,
}

/// Everything the TUI derives from the on-disk config, loaded before any
/// input can reach the loop (discovery surfaces never do config I/O).
pub struct TuiSettings {
    /// The plugin-merged keybindings snapshot. Every load path fills it (a
    /// config that does not load falls back to the shipped defaults); `None`
    /// is left for tests that build settings by hand.
    pub keybindings: Option<KeybindingsCfg>,
    /// The keybind resolver built from that snapshot. `None`
    /// exactly when [`Self::keybindings`] is.
    pub resolver: Option<Resolver>,
    /// Single source of truth for chrome + overlay colors.
    pub theme: Theme,
    /// The responsive-chrome thresholds.
    pub chrome: ChromeBreakpoints,
    /// Status-bar painter, or `None` when the config composes an
    /// empty bar (the driver then reclaims the bar row).
    pub status_bar: Option<StatusBarPainter>,
    /// Palette rows + manifest `keys` merged into the prefix
    /// table.
    pub plugin_actions: Vec<PluginActionEntry>,
    /// The hostable pane entries committing `plugin-pane`.
    pub plugin_panes: Vec<PluginPaneEntry>,
    /// Enabled plugins' `[[sidebar]]` sections (ADR-0148).
    pub plugin_sidebar: Vec<PluginSectionSpec>,
    /// The which-key popup knobs.
    pub which_key: WhichKey,
    /// `[sidebar]` geometry; attach-time only.
    pub sidebar: SidebarSettings,
    /// `[sidebar]` hosts keys: the machine-segment provider; attach-time only.
    pub hosts: crate::attach::hosts::HostsSettings,
    /// The global `defaults.mouse` gate the `RawModeGuard` install reads;
    /// attach-time only.
    pub mouse_capture: bool,
    /// `defaults.clipboard-write`: what a pane's OSC 52 write does
    /// (ADR-0158). Live on reload.
    pub clipboard_write: phux_config::ClipboardWrite,
    /// `[notifications]`: agent notification delivery. Live on reload.
    pub notifications: phux_config::NotificationsCfg,
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
            .field("plugin_sidebar", &self.plugin_sidebar.len())
            .field("which_key", &self.which_key)
            .field("sidebar", &self.sidebar)
            .field("hosts", &self.hosts)
            .field("mouse_capture", &self.mouse_capture)
            .field("clipboard_write", &self.clipboard_write)
            .field("notifications", &self.notifications)
            .finish()
    }
}

impl TuiSettings {
    /// Attach-time load with every failure degraded; a file that does not
    /// load yields the shipped defaults and the load error on the bar.
    #[must_use]
    pub fn load_tolerant() -> Self {
        match phux_config::loader::load() {
            Ok(cfg) => Self::tolerant_from(&cfg),
            Err(err) => Self::without_config(&err),
        }
    }

    /// Reload: re-read the layered config at `path` strictly (same loader as
    /// attach).
    ///
    /// # Errors
    ///
    /// A single-problem message for a toast; never a partial result.
    pub fn load_strict(path: &Path) -> Result<Self, String> {
        let cfg = phux_config::loader::load_from(path).map_err(|err| err.to_string())?;
        Self::strict_from(&cfg)
    }

    /// Re-read `path` and swap the reloadable subset in, or leave `self`
    /// untouched.
    ///
    /// # Errors
    ///
    /// The [`Self::load_strict`] message.
    pub fn reload_in_place(&mut self, path: &Path) -> Result<(), String> {
        let new = Self::load_strict(path)?;
        self.adopt_reload(new);
        Ok(())
    }

    /// Take the reloadable subset from `new` (sidebar geometry and the mouse
    /// gate take effect on the next attach).
    pub fn adopt_reload(&mut self, new: Self) {
        self.keybindings = new.keybindings;
        self.resolver = new.resolver;
        self.theme = new.theme;
        self.chrome = new.chrome;
        self.status_bar = new.status_bar;
        self.plugin_actions = new.plugin_actions;
        self.plugin_panes = new.plugin_panes;
        self.plugin_sidebar = new.plugin_sidebar;
        self.which_key = new.which_key;
        self.clipboard_write = new.clipboard_write;
        self.notifications = new.notifications;
    }

    /// The seed when the config does not load: the shipped defaults, keys
    /// included (so `C-a d` still detaches), with the load error on the bar
    /// row instead of the configured status line.
    fn without_config(err: &ConfigError) -> Self {
        tracing::warn!(error = %err, "phux-config load failed; using shipped defaults");
        let defaults = phux_config::parse_with_defaults("", &phux_config::loader::config_path())
            .unwrap_or_else(|default_err| {
                tracing::error!(error = %default_err, "shipped defaults failed to parse");
                Config::default()
            });
        let mut settings = Self::tolerant_from(&defaults);
        let mut status_bar = StatusBarPainter::error_line(config_error_line(err));
        status_bar.set_theme(&settings.theme);
        settings.status_bar = Some(status_bar);
        settings
    }

    /// Build tolerantly: the bar degrades to the error-line painter, the
    /// resolver is lenient, and its diagnostics take the bar row unless a
    /// config error already does.
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
        let plugin_sidebar = plugin_sidebar::specs_from_manifests(&manifests);
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
        let theme = Theme::from_cfg(&cfg.theme, &manifests);
        Self::assemble(
            cfg,
            keybindings,
            resolver,
            status_bar,
            plugin_actions,
            plugin_panes,
            plugin_sidebar,
        )
        .with_theme(theme)
    }

    /// Build strictly: any widget or binding failure fails the build.
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
        let plugin_sidebar = plugin_sidebar::specs_from_manifests(&manifests);
        let keybindings = merged_keybindings(cfg, &plugin_actions);
        // Strict on purpose: reload is all-or-nothing. The refusal names the
        // binding, so the toast says which line of the file to fix.
        let (resolver, diagnostics) = Resolver::new_lenient(&keybindings);
        if let Some(diagnostic) = diagnostics.first() {
            return Err(diagnostic.to_string());
        }
        // A `[theme] name` that resolves to nothing is a config error too.
        let theme = Theme::resolve(&cfg.theme, &manifests).map_err(|err| err.to_string())?;
        Ok(Self::assemble(
            cfg,
            keybindings,
            resolver,
            status_bar,
            plugin_actions,
            plugin_panes,
            plugin_sidebar,
        )
        .with_theme(theme))
    }

    /// Adopt `theme`, including the status bar's theme-derived colours: the
    /// attention chip rides the `attention` slot rather than a hardcoded SGR.
    fn with_theme(mut self, theme: Theme) -> Self {
        if let Some(sb) = self.status_bar.as_mut() {
            sb.set_theme(&theme);
        }
        self.theme = theme;
        self
    }

    /// The policy-independent tail of both builds; [`Self::with_theme`]
    /// supplies the theme, which the two builds resolve differently.
    fn assemble(
        cfg: &Config,
        keybindings: KeybindingsCfg,
        resolver: Resolver,
        status_bar: Option<StatusBarPainter>,
        plugin_actions: Vec<PluginActionEntry>,
        plugin_panes: Vec<PluginPaneEntry>,
        plugin_sidebar: Vec<PluginSectionSpec>,
    ) -> Self {
        Self {
            which_key: WhichKey {
                enabled: keybindings.which_key,
                delay: Duration::from_millis(keybindings.which_key_delay_ms),
            },
            keybindings: Some(keybindings),
            resolver: Some(resolver),
            theme: Theme::default(),
            chrome: ChromeBreakpoints::from_cfg(&cfg.chrome),
            status_bar,
            plugin_actions,
            plugin_panes,
            plugin_sidebar,
            sidebar: SidebarSettings {
                enabled: cfg.sidebar.enabled,
                width: cfg.sidebar.width,
                edge: sidebar_edge(cfg.sidebar.position),
                agents_share: cfg.sidebar.agents_share,
            },
            hosts: crate::attach::hosts::HostsSettings::from_cfg(&cfg.sidebar),
            mouse_capture: cfg.defaults.mouse,
            clipboard_write: cfg.defaults.clipboard_write,
            notifications: cfg.notifications,
        }
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

/// Enabled plugins' manifests, resolved like `phux config run` does; broken
/// ones are skipped with a warning.
fn enabled_manifests(cfg: &Config) -> Vec<PluginManifest> {
    if cfg.plugins.is_empty() {
        return Vec::new();
    }
    phux_config::plugin::load_enabled_manifests(&phux_config::loader::config_path(), &cfg.plugins)
}

/// Keybindings with plugin `keys` merged in (user config wins), cached.
fn merged_keybindings(cfg: &Config, plugin_actions: &[PluginActionEntry]) -> KeybindingsCfg {
    let mut kb = cfg.keybindings.clone();
    plugin_actions::merge_plugin_bindings(&mut kb, plugin_actions);
    kb
}

/// Compose the status-bar painter from a config plus enabled plugins'
/// manifests.
///
/// The one composition point shared by the tolerant and strict builds (they
/// once drifted, resetting `position` on reload). Plugin widgets merge after
/// the user's own. `Ok(None)` ⇒ an empty bar.
///
/// # Errors
///
/// The [`phux_config::widget::StatusBar::build`] error, for the caller's
/// policy.
pub fn compose_status_bar(
    cfg: &Config,
    manifests: &[PluginManifest],
) -> Result<Option<StatusBarPainter>, WidgetError> {
    let registry = phux_config::WidgetRegistry::with_builtins();
    // Invalid plugin contributions are dropped with a warning inside the
    // merge; a broken user config still fails the build.
    let mut status = cfg.status.clone();
    phux_config::widget::merge_widget_contributions(&mut status, manifests, &registry);
    let bar = phux_config::widget::StatusBar::build(&status, &registry)?;
    if bar.is_empty() {
        return Ok(None);
    }
    let mut painter = StatusBarPainter::new(bar, cfg.status.position.into());
    painter.set_prefix(cfg.keybindings.prefix.clone());
    Ok(Some(painter))
}

/// Build the lenient [`Resolver`] from the plugin-merged snapshot: each
/// diagnostic disables only the binding it names (one bad chord once
/// disabled every binding, `detach` included). Reload stays strict.
#[must_use]
pub fn build_resolver_from(kb: &KeybindingsCfg) -> (Resolver, Vec<BindingDiagnostic>) {
    let (resolver, diagnostics) = Resolver::new_lenient(kb);
    for diag in &diagnostics {
        tracing::warn!(binding = %diag.binding, error = %diag.error, "keybinding disabled");
    }
    (resolver, diagnostics)
}

/// The lenient resolver's diagnostics as a one-line bar error: the first
/// chord, the reason, how many more, and `phux config check`. Empty input
/// formats empty.
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

/// A one-line config error for the bar, pointing at `phux config check`
/// (which diagnoses; `config show` only renders).
pub fn config_error_line(err: &impl std::fmt::Display) -> String {
    format!("config error: {err} (run: phux config check)")
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
#[path = "settings_tests.rs"]
mod tests;
