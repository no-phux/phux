//! Live config reload (phux-foz.5).
//!
//! The driver keeps a set of config-derived state — the plugin-merged
//! keybindings snapshot, the keybind resolver, the color theme, the
//! status-bar painter, the plugin-action palette rows, and the which-key
//! knobs. This module rebuilds that whole set from a fresh run of the
//! layered config loader ([`phux_config::loader::load_from`], so
//! `extends` stacks resolve exactly as at startup) and swaps it in
//! **atomically**: a parse or validation failure leaves every piece of
//! the previous configuration in place and returns the error for the
//! driver to surface. Nothing is ever half-applied.
//!
//! Reloads are explicit — the `reload-config` action (palette row or a
//! user-bound chord) or the `phux config reload` CLI doorbell — never
//! file-watched. See `docs/consumers/tui.md` §4.3 for the rationale.

use std::path::Path;
use std::time::Duration;

use phux_config::keybind::Resolver;
use phux_config::{Config, KeybindingsCfg};

use super::plugin_actions::{self, PluginActionEntry};
use super::plugin_panes::{self, PluginPaneEntry};
use crate::render::Theme;
use crate::render::chrome::status_bar::StatusBarPainter;

/// The full set of config-derived driver state a reload replaces.
///
/// Built as one unit so the swap is all-or-nothing: either every field
/// comes from the same freshly-validated [`Config`], or (on any error)
/// none of the driver's existing state is touched.
pub(super) struct ReloadedConfig {
    /// Plugin-merged keybindings snapshot (help overlay, palette chords,
    /// which-key rows).
    pub keybindings: KeybindingsCfg,
    /// Keybind resolver built from [`Self::keybindings`].
    pub resolver: Resolver,
    /// Chrome + overlay color theme.
    pub theme: Theme,
    /// Status-bar painter, or `None` when the config composes an empty
    /// bar (the driver then reclaims the bar row).
    pub status_bar: Option<StatusBarPainter>,
    /// Enabled plugins' manifest `[[actions]]` (palette "Plugin" rows).
    pub plugin_actions: Vec<PluginActionEntry>,
    /// Enabled plugins' manifest `[[panes]]` (palette "Plugin" rows that
    /// commit `plugin-pane`; phux-r82.7).
    pub plugin_panes: Vec<PluginPaneEntry>,
    /// `[keybindings].which-key` toggle.
    pub which_key_enabled: bool,
    /// `[keybindings].which-key-delay-ms`, as a [`Duration`].
    pub which_key_delay: Duration,
}

/// Re-read the layered config at `path` and rebuild every piece of
/// config-derived driver state from it.
///
/// This is the same loader the driver runs at startup, so `extends`
/// stacks, `-append` array merges, and the embedded defaults all apply
/// identically. Unlike the tolerant startup path (which degrades a bad
/// status bar or resolver to a warning so a broken config never blocks
/// attach), a reload is **strict**: any parse or validation failure —
/// unreadable file, malformed TOML, a broken layer stack, a widget the
/// status bar cannot build, a keybinding table the resolver rejects —
/// fails the whole reload so the caller keeps its previous state.
///
/// # Errors
///
/// A human-readable, single-config-problem message suitable for an
/// overlay ("what is wrong with the file"), never a partial result.
pub(super) fn reload_from(path: &Path) -> Result<ReloadedConfig, String> {
    let cfg = phux_config::loader::load_from(path).map_err(|err| err.to_string())?;
    build(&cfg)
}

/// Compose the status-bar painter from a parsed [`Config`] plus the
/// enabled plugins' manifests.
///
/// This is the ONE composition point shared by the startup path
/// (`driver::build_status_bar_painter`) and the reload path ([`build`]),
/// extracted so the two cannot drift again (phux-i0e8.6.1: reload used
/// to hardcode the default position and skip
/// [`phux_config::plugin::merge_widget_contributions`], silently
/// resetting `[status] position = "top"` and dropping plugin-contributed
/// widgets). The order mirrors startup exactly: plugin `[[widgets]]`
/// contributions merge in **after** the user's own `[status]` widgets
/// and **before** the bar builds, and the painter takes
/// `cfg.status.position` and the configured prefix. An `Ok(None)` means
/// the merged config composes an empty bar (the caller then reclaims
/// the bar row).
///
/// Only the composition is shared; error POLICY stays with the caller:
/// startup degrades a build failure to the error-line painter so a
/// broken config never blocks attach, while a reload fails atomically
/// and keeps the previous config.
///
/// # Errors
///
/// Forwards the [`phux_config::widget::StatusBar::build`] error (unknown
/// widget kind, bad option) untouched for the caller's policy.
pub(super) fn compose_status_bar(
    cfg: &Config,
    manifests: &[phux_config::plugin::PluginManifest],
) -> Result<Option<StatusBarPainter>, phux_config::widget::WidgetError> {
    let registry = phux_config::WidgetRegistry::with_builtins();
    // phux-r82.6: fold enabled plugins' `[[widgets]]` contributions in
    // after the user's own `[status]` widgets. Invalid contributions are
    // dropped with a warning inside the merge (mirroring the plugin
    // keybinding policy), so a broken plugin cannot fail the build; a
    // genuinely broken USER config still can.
    let mut status = cfg.status.clone();
    phux_config::plugin::merge_widget_contributions(&mut status, manifests, &registry);
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

/// Build a [`ReloadedConfig`] from an already-parsed [`Config`].
///
/// Split from [`reload_from`] so tests can drive it with
/// [`phux_config::parse_with_defaults`] output directly.
fn build(cfg: &Config) -> Result<ReloadedConfig, String> {
    // Plugin manifests load FIRST (startup parity, phux-i0e8.6.1),
    // relative to the canonical config path exactly like the startup
    // path: the status bar must compose with plugin `[[widgets]]`
    // contributions merged in, and actions + their manifest `keys`
    // merge like at startup so a chord resolves the same before and
    // after reload (user config wins every conflict). The hostable pane
    // rows (phux-r82.7) refresh from the same snapshot.
    let manifests = phux_config::plugin::load_enabled_manifests(
        &phux_config::loader::config_path(),
        &cfg.plugins,
    );

    // Status bar next: widget composition is the one post-parse
    // validation step that can still reject the config.
    let status_bar = compose_status_bar(cfg, &manifests).map_err(|err| err.to_string())?;

    let plugin_actions = plugin_actions::entries_from_manifests(&manifests);
    let plugin_panes = plugin_panes::entries_from_manifests(&manifests);
    let mut keybindings = cfg.keybindings.clone();
    plugin_actions::merge_plugin_bindings(&mut keybindings, &plugin_actions);

    // phux-i0e8.3.4: deliberately the STRICT build. Reload keeps its
    // all-or-nothing contract (docs/consumers/tui.md §4.3): any binding
    // the resolver rejects fails the whole reload and the previous
    // config stays fully in effect. This is the intended asymmetry with
    // attach, where `build_resolver_from` (driver.rs) builds leniently —
    // at attach there is no known-good previous config to keep, so a bad
    // binding degrades per-binding with a status-bar diagnostic instead.
    let resolver = Resolver::new(&keybindings).map_err(|err| err.to_string())?;
    let theme = Theme::from_cfg(&cfg.theme);
    let mut reloaded = ReloadedConfig {
        which_key_enabled: keybindings.which_key,
        which_key_delay: Duration::from_millis(keybindings.which_key_delay_ms),
        keybindings,
        resolver,
        theme,
        status_bar,
        plugin_actions,
        plugin_panes,
    };
    // phux-foz.1: the attention chip color rides the theme.
    if let Some(sb) = reloaded.status_bar.as_mut() {
        sb.set_attention_color(reloaded.theme.attention);
    }
    Ok(reloaded)
}

/// Reload from `path` and swap the new state into the driver's slots —
/// or, on any failure, leave every slot untouched and hand back the
/// error message.
///
/// This is the "keep the old config, never half-apply" contract in one
/// place: the swap happens only after [`reload_from`] returned a fully
/// built [`ReloadedConfig`].
///
/// # Errors
///
/// The [`reload_from`] error message; the out-params are untouched.
#[allow(
    clippy::too_many_arguments,
    reason = "the slots are driver-loop locals threaded by reference, same shape as the paint helpers"
)]
pub(super) fn reload_in_place(
    path: &Path,
    keybindings_snapshot: &mut Option<KeybindingsCfg>,
    resolver: &mut Option<Resolver>,
    theme: &mut Theme,
    status_bar: &mut Option<StatusBarPainter>,
    plugin_actions: &mut Vec<PluginActionEntry>,
    plugin_panes: &mut Vec<PluginPaneEntry>,
    which_key_enabled: &mut bool,
    which_key_delay: &mut Duration,
) -> Result<(), String> {
    let new = reload_from(path)?;
    *keybindings_snapshot = Some(new.keybindings);
    *resolver = Some(new.resolver);
    *theme = new.theme;
    *status_bar = new.status_bar;
    *plugin_actions = new.plugin_actions;
    *plugin_panes = new.plugin_panes;
    *which_key_enabled = new.which_key_enabled;
    *which_key_delay = new.which_key_delay;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::chrome::status_bar::Position;
    use phux_config::keybind::{Feed, parse_chord};

    /// Write `contents` as a config file inside a fresh temp dir and
    /// return `(dir_guard, config_path)`.
    fn config_file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, contents).expect("write config");
        (dir, path)
    }

    #[test]
    fn reload_applies_keybinding_and_theme_changes() {
        let (_dir, path) = config_file(
            r##"
            [keybindings.prefix-table]
            X = "kill-pane"

            [theme]
            accent = "#ff0000"
            "##,
        );
        let new = reload_from(&path).expect("valid config reloads");
        assert!(
            new.keybindings.prefix_table.contains_key("X"),
            "reload must pick up the new prefix-table binding",
        );
        assert_eq!(
            new.theme.accent,
            ratatui::style::Color::Rgb(0xff, 0, 0),
            "reload must pick up the new theme accent",
        );
        // The resolver is built from the same snapshot: the new chord
        // resolves (prefix, then X) to the bound action.
        let mut resolver = new.resolver;
        let prefix = parse_chord(&new.keybindings.prefix).expect("prefix parses");
        assert_eq!(resolver.feed(prefix), Feed::Partial);
        match resolver.feed(parse_chord("X").expect("chord parses")) {
            Feed::Resolved(ra) => assert_eq!(ra.action, "kill-pane"),
            other => panic!("expected the reloaded binding to resolve, got {other:?}"),
        }
    }

    #[test]
    fn reload_reads_which_key_knobs() {
        let (_dir, path) = config_file(
            r"
            [keybindings]
            which-key = false
            which-key-delay-ms = 250
            ",
        );
        let new = reload_from(&path).expect("valid config reloads");
        assert!(!new.which_key_enabled);
        assert_eq!(new.which_key_delay, Duration::from_millis(250));
    }

    #[test]
    fn failed_reload_keeps_previous_config() {
        // Seed "previous" state clearly distinguishable from defaults.
        let mut keybindings = Some(KeybindingsCfg::default());
        let mut resolver = None;
        let mut theme = Theme {
            accent: ratatui::style::Color::Rgb(1, 2, 3),
            ..Theme::default()
        };
        let old_theme = theme;
        let mut status_bar = None;
        let mut plugin_actions = vec![];
        let mut plugin_panes = vec![];
        let mut which_key_enabled = false;
        let mut which_key_delay = Duration::from_millis(123);

        let (_dir, path) = config_file("this is [not valid toml");
        let err = reload_in_place(
            &path,
            &mut keybindings,
            &mut resolver,
            &mut theme,
            &mut status_bar,
            &mut plugin_actions,
            &mut plugin_panes,
            &mut which_key_enabled,
            &mut which_key_delay,
        )
        .expect_err("malformed TOML must fail the reload");
        assert!(!err.is_empty(), "error message must be surfaceable");

        // Every slot is exactly as it was: nothing half-applied.
        assert!(keybindings.is_some());
        assert!(resolver.is_none());
        assert_eq!(theme, old_theme);
        assert!(status_bar.is_none());
        assert!(plugin_actions.is_empty());
        assert!(plugin_panes.is_empty());
        assert!(!which_key_enabled);
        assert_eq!(which_key_delay, Duration::from_millis(123));
    }

    #[test]
    fn reload_honors_status_position_top() {
        // phux-i0e8.6.1 regression: the reload path used to construct
        // the painter with `Position::default()`, silently dropping the
        // user's `position = "top"` back to bottom on every reload.
        let (_dir, path) = config_file(
            r#"
            [status]
            position = "top"
            "#,
        );
        let new = reload_from(&path).expect("valid config reloads");
        let painter = new
            .status_bar
            .expect("default widgets keep the bar non-empty");
        assert_eq!(
            painter.position(),
            Position::Top,
            "position = \"top\" must survive a reload",
        );
    }

    #[test]
    fn reload_keeps_plugin_widget_contributions() {
        // phux-i0e8.6.1 regression: the reload path used to build the
        // bar BEFORE loading plugin manifests and never ran
        // merge_widget_contributions, dropping plugin `[[widgets]]` on
        // every reload. With the user's own slots explicitly empty, the
        // bar is non-empty after reload ONLY if the plugin contribution
        // was merged in before the build.
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest_path = dir.path().join("phux-plugin.toml");
        std::fs::write(
            &manifest_path,
            r#"
            id = "com.example.clock"
            name = "Clock"
            version = "0.1.0"
            min_phux_version = "0.0.1"

            [[widgets]]
            id = "clock"
            slot = "right"
            kind = "time"
            "#,
        )
        .expect("write manifest");
        // Absolute manifest path: reload resolves relative paths against
        // the canonical config path, not the test file's directory.
        let (_cfg_dir, config_path) = config_file(&format!(
            r#"
            [status]
            position = "top"
            left = []
            center = []
            right = []

            [[plugins]]
            manifest = {manifest:?}
            "#,
            manifest = manifest_path.display().to_string(),
        ));
        let new = reload_from(&config_path).expect("valid config reloads");
        let painter = new
            .status_bar
            .expect("plugin-contributed widget must keep the bar non-empty after reload");
        // Both fixes compose: the plugin widget rides a top-positioned bar.
        assert_eq!(painter.position(), Position::Top);
    }

    #[test]
    fn compose_merges_plugin_widgets_before_build() {
        // The shared composition helper (used by both the startup and
        // reload paths) must merge manifest `[[widgets]]` contributions
        // into the bar: with the user's slots empty, the manifests are
        // the only thing that can make the bar non-empty.
        let cfg = phux_config::parse_with_defaults(
            r"
            [status]
            left = []
            center = []
            right = []
            ",
            std::path::Path::new("test-config.toml"),
        )
        .expect("config parses");
        assert!(
            compose_status_bar(&cfg, &[])
                .expect("empty bar composes")
                .is_none(),
            "no user widgets + no manifests must compose an empty bar",
        );

        let manifest = phux_config::plugin::PluginManifest {
            id: "com.example.clock".to_owned(),
            name: "Clock".to_owned(),
            version: "0.1.0".to_owned(),
            min_phux_version: "0.0.1".to_owned(),
            description: None,
            manifest_path: std::path::PathBuf::from("/x/phux-plugin.toml"),
            plugin_root: std::path::PathBuf::from("/x"),
            platforms: None,
            build: Vec::new(),
            agents: Vec::new(),
            actions: Vec::new(),
            events: Vec::new(),
            panes: Vec::new(),
            links: Vec::new(),
            workspaces: Vec::new(),
            widgets: vec![phux_config::plugin::PluginManifestWidget {
                id: "clock".to_owned(),
                slot: phux_config::plugin::PluginWidgetSlot::Right,
                kind: "time".to_owned(),
                opts: std::collections::BTreeMap::new(),
            }],
        };
        assert!(
            compose_status_bar(&cfg, &[manifest])
                .expect("contributed bar composes")
                .is_some(),
            "a manifest [[widgets]] contribution must survive composition",
        );
    }

    #[test]
    fn invalid_widget_fails_reload_atomically() {
        // Parseable TOML, but the status bar names a widget the registry
        // does not know: post-parse validation must fail the reload.
        let (_dir, path) = config_file(
            r#"
            [status]
            left = ["no-such-widget"]
            "#,
        );
        assert!(
            reload_from(&path).is_err(),
            "unknown widget must fail the whole reload, not degrade",
        );
    }

    #[test]
    fn successful_reload_swaps_every_slot() {
        let mut keybindings = None;
        let mut resolver = None;
        let mut theme = Theme::default();
        let mut status_bar = None;
        let mut plugin_actions = vec![];
        let mut plugin_panes = vec![];
        let mut which_key_enabled = true;
        let mut which_key_delay = Duration::from_millis(600);

        let (_dir, path) = config_file(
            r#"
            [keybindings]
            which-key-delay-ms = 150

            [keybindings.prefix-table]
            Y = "toggle-zoom"
            "#,
        );
        reload_in_place(
            &path,
            &mut keybindings,
            &mut resolver,
            &mut theme,
            &mut status_bar,
            &mut plugin_actions,
            &mut plugin_panes,
            &mut which_key_enabled,
            &mut which_key_delay,
        )
        .expect("valid config applies");
        assert!(
            keybindings
                .as_ref()
                .is_some_and(|kb| kb.prefix_table.contains_key("Y")),
            "snapshot must carry the reloaded binding",
        );
        assert!(resolver.is_some(), "resolver must be rebuilt");
        assert_eq!(which_key_delay, Duration::from_millis(150));
    }
}
