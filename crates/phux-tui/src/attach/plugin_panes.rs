//! Plugin manifest `[[panes]]` in the TUI.
//!
//! A declared pane opens its argv in a real server Terminal through the same
//! `SPAWN_RESOURCE` verb `split-pane` and `new-window` use (plugin root as
//! cwd, `PHUX_PLUGIN_*` env), with no new wire surface (ADR-0017).
//!
//! Placement: `split` parks a `PendingSplit`, `tab` a `PendingWindow` named
//! after the pane, `zoomed` a split that zooms on spawn, and `overlay` a
//! floating box over the pane area that is in no window (ADR-0147, see
//! `attach::floating`). Palette rows commit [`PLUGIN_PANE_NAME`] with
//! `plugin`/`pane` args.

use std::path::PathBuf;

use phux_config::keybind::ResolvedAction;
use phux_config::plugin::{PluginManifest, PluginPanePlacement};
use phux_protocol::wire::frame::FrameKind;

use phux_client::layout_ops::DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID;

/// The dispatcher action plugin pane rows commit (dynamic, so exempt from
/// the static registry).
pub const PLUGIN_PANE_NAME: &str = "plugin-pane";

/// Where a hosted plugin pane opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedPlacement {
    /// Split beside the focused pane (side-by-side).
    Split,
    /// New window ("tab") named after the pane's title.
    Tab,
    /// Split beside the focused pane, then zoom the new pane to fill
    /// the window.
    Zoomed,
    /// A modal box over the pane area, in no window (ADR-0147).
    Overlay,
}

/// One enabled plugin pane the TUI can host, snapshotted at driver start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPaneEntry {
    /// Configured plugin id (manifest `id`).
    pub plugin_id: String,
    /// Human-readable plugin name (manifest `name`).
    pub plugin_name: String,
    /// Plugin-local pane id.
    pub pane_id: String,
    /// Human-readable pane title.
    pub title: String,
    /// Hosted placement.
    pub placement: HostedPlacement,
    /// Command argv the spawned Terminal runs.
    pub command: Vec<String>,
    /// Directory containing the manifest; the spawn's working directory
    /// and the `PHUX_PLUGIN_ROOT` value (matching the action runtime).
    pub plugin_root: PathBuf,
}

impl PluginPaneEntry {
    /// The palette row label: namespaced so pane rows can't be mistaken
    /// for built-in actions or plugin *action* rows.
    #[must_use]
    pub fn palette_label(&self) -> String {
        format!("plugin pane: {}: {}", self.plugin_name, self.title)
    }

    /// The [`ResolvedAction`] this entry commits — the same shape a
    /// keybinding or palette row produces, flowing through `run_action`.
    #[must_use]
    pub fn resolved_action(&self) -> ResolvedAction {
        let mut args = std::collections::BTreeMap::new();
        args.insert(
            "plugin".to_owned(),
            toml::Value::String(self.plugin_id.clone()),
        );
        args.insert("pane".to_owned(), toml::Value::String(self.pane_id.clone()));
        ResolvedAction {
            action: PLUGIN_PANE_NAME.to_owned(),
            args,
        }
    }

    /// The additive env for the spawned Terminal: `PHUX_PLUGIN_ID`,
    /// `PHUX_PLUGIN_ROOT`, and `PHUX_PLUGIN_PANE_ID`.
    #[must_use]
    pub fn spawn_env(&self) -> Vec<(String, String)> {
        vec![
            ("PHUX_PLUGIN_ID".to_owned(), self.plugin_id.clone()),
            ("PHUX_PLUGIN_PANE_ID".to_owned(), self.pane_id.clone()),
            (
                "PHUX_PLUGIN_ROOT".to_owned(),
                self.plugin_root.display().to_string(),
            ),
        ]
    }

    /// The `SPAWN_RESOURCE` frame that opens this pane: manifest argv, plugin
    /// root as cwd, [`spawn_env`](Self::spawn_env) as env.
    #[must_use]
    pub fn spawn_frame(&self, request_id: u32) -> FrameKind {
        FrameKind::SpawnResource {
            request_id,
            group: DEFAULT_GROUP_ID,
            command: Some(self.command.clone()),
            cwd: Some(self.plugin_root.display().to_string()),
            env: Some(self.spawn_env()),
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            // Geometry depends on the placement the caller
            // chooses, which this entry does not decide. `run_action` fills
            // it in from the tile it is about to park.
            initial_size: None,
            resource: None,
        }
    }
}

/// Flatten loaded manifests into hostable pane entries (pure). Empty argv is
/// dropped with a warning.
#[must_use]
pub fn entries_from_manifests(manifests: &[PluginManifest]) -> Vec<PluginPaneEntry> {
    let mut entries = Vec::new();
    for manifest in manifests {
        for pane in &manifest.panes {
            let placement = match pane.placement {
                PluginPanePlacement::Split => HostedPlacement::Split,
                PluginPanePlacement::Tab => HostedPlacement::Tab,
                PluginPanePlacement::Zoomed => HostedPlacement::Zoomed,
                PluginPanePlacement::Overlay => HostedPlacement::Overlay,
            };
            if pane.command.is_empty() {
                tracing::warn!(
                    plugin = %manifest.id,
                    pane = %pane.id,
                    "plugin pane declares an empty command; skipping entry",
                );
                continue;
            }
            entries.push(PluginPaneEntry {
                plugin_id: manifest.id.clone(),
                plugin_name: manifest.name.clone(),
                pane_id: pane.id.clone(),
                title: pane.title.clone(),
                placement,
                command: pane.command.clone(),
                plugin_root: manifest.plugin_root.clone(),
            });
        }
    }
    entries
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_config::plugin::PluginManifestPane;

    fn pane(id: &str, placement: PluginPanePlacement, command: Vec<String>) -> PluginManifestPane {
        PluginManifestPane {
            id: id.to_owned(),
            title: format!("{id} title"),
            description: None,
            platforms: None,
            placement,
            command,
        }
    }

    fn manifest(id: &str, panes: Vec<PluginManifestPane>) -> PluginManifest {
        PluginManifest {
            id: id.to_owned(),
            name: format!("{id} name"),
            version: "0.1.0".to_owned(),
            min_phux_version: "0.0.2".to_owned(),
            description: None,
            manifest_path: PathBuf::from("/x/phux-plugin.toml"),
            plugin_root: PathBuf::from("/x"),
            platforms: None,
            build: Vec::new(),
            agents: Vec::new(),
            actions: Vec::new(),
            events: Vec::new(),
            panes,
            links: Vec::new(),
            workspaces: Vec::new(),
            widgets: Vec::new(),
            sidebar: Vec::new(),
            themes: Vec::new(),
        }
    }

    #[test]
    fn entries_map_split_tab_zoomed_placements() {
        let m = manifest(
            "p",
            vec![
                pane("a", PluginPanePlacement::Split, vec!["cmd-a".to_owned()]),
                pane("b", PluginPanePlacement::Tab, vec!["cmd-b".to_owned()]),
                pane("c", PluginPanePlacement::Zoomed, vec!["cmd-c".to_owned()]),
            ],
        );
        let entries = entries_from_manifests(std::slice::from_ref(&m));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].placement, HostedPlacement::Split);
        assert_eq!(entries[1].placement, HostedPlacement::Tab);
        assert_eq!(entries[2].placement, HostedPlacement::Zoomed);
        assert_eq!(entries[0].palette_label(), "plugin pane: p name: a title");
    }

    #[test]
    fn overlay_placement_is_hosted() {
        let m = manifest(
            "p",
            vec![
                pane("ov", PluginPanePlacement::Overlay, vec!["x".to_owned()]),
                pane("s", PluginPanePlacement::Split, vec!["y".to_owned()]),
            ],
        );
        let entries = entries_from_manifests(std::slice::from_ref(&m));
        assert_eq!(entries.len(), 2, "overlay and split both host");
        assert_eq!(entries[0].placement, HostedPlacement::Overlay);
        assert_eq!(entries[0].palette_label(), "plugin pane: p name: ov title");
    }

    #[test]
    fn empty_command_is_skipped() {
        let m = manifest("p", vec![pane("e", PluginPanePlacement::Split, Vec::new())]);
        assert!(entries_from_manifests(std::slice::from_ref(&m)).is_empty());
    }

    #[test]
    fn spawn_frame_carries_argv_cwd_and_identity_env() {
        let m = manifest(
            "com.example.board",
            vec![pane(
                "board",
                PluginPanePlacement::Split,
                vec!["agent-board".to_owned(), "--watch".to_owned()],
            )],
        );
        let entry = &entries_from_manifests(std::slice::from_ref(&m))[0];
        let FrameKind::SpawnResource {
            request_id,
            group,
            command,
            cwd,
            env,
            term,
            satellite,
            owner_terminal,
            agent_session,
            initial_size,
            ..
        } = entry.spawn_frame(7)
        else {
            panic!("expected SpawnResource");
        };
        assert_eq!(request_id, 7);
        assert_eq!(
            initial_size, None,
            "the manifest entry does not know its placement, so run_action fills the tile in",
        );
        assert_eq!(satellite, None, "plugin panes spawn locally");
        assert_eq!(owner_terminal, None, "plugin panes use attached ownership");
        assert_eq!(
            agent_session, None,
            "plugin panes have no resume provenance"
        );
        assert_eq!(group, DEFAULT_GROUP_ID);
        assert_eq!(
            command,
            Some(vec!["agent-board".to_owned(), "--watch".to_owned()])
        );
        assert_eq!(cwd.as_deref(), Some("/x"));
        assert_eq!(term, None);
        let env = env.expect("identity env injected");
        assert!(
            env.contains(&("PHUX_PLUGIN_ID".to_owned(), "com.example.board".to_owned())),
            "env was {env:?}"
        );
        assert!(env.contains(&("PHUX_PLUGIN_PANE_ID".to_owned(), "board".to_owned())));
        assert!(env.contains(&("PHUX_PLUGIN_ROOT".to_owned(), "/x".to_owned())));
    }

    #[test]
    fn disabled_plugin_contributes_no_pane_entries() {
        // End-to-end through the same loader the driver uses: two on-disk
        // manifests, one disabled. Only the enabled plugin's panes
        // survive.
        let dir = tempfile::tempdir().expect("tempdir");
        let write_manifest = |name: &str, id: &str| {
            let root = dir.path().join(name);
            std::fs::create_dir_all(&root).expect("plugin dir");
            let path = root.join("phux-plugin.toml");
            let body = format!(
                "id = \"{id}\"\n\
                 name = \"{id}\"\n\
                 version = \"0.1.0\"\n\
                 min_phux_version = \"0.0.2\"\n\
                 [[panes]]\n\
                 id = \"board\"\n\
                 title = \"Board\"\n\
                 placement = \"split\"\n\
                 command = [\"board\"]\n"
            );
            std::fs::write(&path, body).expect("write manifest");
            path
        };
        let enabled_path = write_manifest("on", "com.example.on");
        let disabled_path = write_manifest("off", "com.example.off");
        let entries = vec![
            phux_config::plugin::PluginConfigEntry {
                manifest: enabled_path,
                enabled: true,
            },
            phux_config::plugin::PluginConfigEntry {
                manifest: disabled_path,
                enabled: false,
            },
        ];
        let config_path = dir.path().join("config.toml");
        let manifests = phux_config::plugin::load_enabled_manifests(&config_path, &entries);
        let panes = entries_from_manifests(&manifests);
        assert_eq!(panes.len(), 1, "only the enabled plugin's pane hosts");
        assert_eq!(panes[0].plugin_id, "com.example.on");
    }
}
