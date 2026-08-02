//! Plugin pane host in the TUI (phux-r82.7).
//!
//! Plugin manifest `[[panes]]` declare a command plus a `placement`
//! (`overlay | split | tab | zoomed`) but were previously inert metadata —
//! nothing opened them. This module makes a declared pane actually open,
//! running its argv inside a real server-side Terminal.
//!
//! ## No new wire surface (ADR-0017)
//!
//! The TUI is not protocol-privileged: a plugin pane opens through the
//! SAME `SPAWN_TERMINAL` verb the TUI's own `split-pane` / `new-window`
//! actions use, with the manifest's argv as the spawn `command`, the
//! plugin root as `cwd`, and the `PHUX_PLUGIN_*` identity variables as
//! additive `env` entries (mirroring the `phux-plugin` action runtime's
//! injection). Any consumer could do exactly this; nothing here touches
//! `phux-protocol`.
//!
//! ## Placement routing
//!
//! * `split`  — spawn + park a `PendingSplit` (see `super::actions`)
//!   against the focused pane (side-by-side, like the palette's
//!   `split-pane` default).
//! * `tab`    — spawn + park a `PendingWindow` (see `super::actions`)
//!   named after the pane's manifest `title`.
//! * `zoomed` — like `split`, but the spawn reply zooms the new pane to
//!   fill the window (`PendingSplit::zoom_on_spawn`); un-zooming reveals
//!   it tiled beside the anchor pane.
//! * `overlay` — **deferred.** An overlay hosting a live terminal needs a
//!   floating pane surface the chrome layer does not have yet; entries
//!   declaring it are skipped at snapshot time with a logged warning (see
//!   `docs/consumers/tui.md` §5.5 and the phux-r82.7 bead notes).
//!
//! The palette lists hosted entries as namespaced rows
//! (`plugin pane: <plugin-name>: <pane title>`) under the shared "Plugin"
//! header; committing one fires the [`PLUGIN_PANE_NAME`] dispatcher action
//! carrying `plugin = <id>, pane = <id>` args, so the palette and any
//! user-configured keybinding share the single `run_action` dispatch path.

use std::path::PathBuf;

use phux_config::keybind::ResolvedAction;
use phux_config::plugin::{PluginManifest, PluginPanePlacement};
use phux_protocol::wire::frame::FrameKind;

use super::driver::DEFAULT_GROUP_ID;

/// The dispatcher action plugin pane palette rows commit.
///
/// Listed in [`super::input_dispatch::ACTION_NAMES`] and handled by a
/// `run_action` arm; exempt from the static palette registry because its
/// rows are built dynamically from the plugin snapshot (same policy as
/// [`super::plugin_actions::PLUGIN_ACTION_NAME`]).
pub const PLUGIN_PANE_NAME: &str = "plugin-pane";

/// Where a hosted plugin pane opens.
///
/// The subset of [`PluginPanePlacement`] the TUI can honor today:
/// `overlay` is deferred (no floating live-terminal surface yet) and
/// never reaches this type — [`entries_from_manifests`] drops it with a
/// warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedPlacement {
    /// Split beside the focused pane (side-by-side).
    Split,
    /// New window ("tab") named after the pane's title.
    Tab,
    /// Split beside the focused pane, then zoom the new pane to fill
    /// the window.
    Zoomed,
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
    /// Hosted placement (overlay entries never make it into a snapshot).
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

    /// The additive environment injected into the spawned Terminal —
    /// the same identity contract as the `phux-plugin` action runtime
    /// (`PHUX_PLUGIN_ID` / `PHUX_PLUGIN_ROOT`), with `PHUX_PLUGIN_PANE_ID`
    /// in place of the action id.
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

    /// Build the `SPAWN_TERMINAL` frame that opens this pane: the
    /// manifest argv as the command, the plugin root as the working
    /// directory, and [`spawn_env`](Self::spawn_env) as additive env.
    ///
    /// This is the existing wire verb the TUI's own `split-pane` /
    /// `new-window` actions use — no plugin-specific protocol surface
    /// (ADR-0017).
    #[must_use]
    pub fn spawn_frame(&self, request_id: u32) -> FrameKind {
        FrameKind::SpawnTerminal {
            request_id,
            group: DEFAULT_GROUP_ID,
            command: Some(self.command.clone()),
            cwd: Some(self.plugin_root.display().to_string()),
            env: Some(self.spawn_env()),
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
        }
    }
}

/// Flatten loaded manifests into hostable pane entries. Pure; separated
/// from config I/O so tests can drive it with in-memory manifests.
///
/// Entries the TUI cannot honestly host are dropped with a
/// `tracing::warn!`, never an error: `placement = "overlay"` (deferred —
/// no floating live-terminal surface yet) and empty argv (nothing to
/// run). Disabled plugins never reach this function — the caller loads
/// manifests via [`phux_config::plugin::load_enabled_manifests`], which
/// skips them.
#[must_use]
pub fn entries_from_manifests(manifests: &[PluginManifest]) -> Vec<PluginPaneEntry> {
    let mut entries = Vec::new();
    for manifest in manifests {
        for pane in &manifest.panes {
            let placement = match pane.placement {
                PluginPanePlacement::Split => HostedPlacement::Split,
                PluginPanePlacement::Tab => HostedPlacement::Tab,
                PluginPanePlacement::Zoomed => HostedPlacement::Zoomed,
                PluginPanePlacement::Overlay => {
                    tracing::warn!(
                        plugin = %manifest.id,
                        pane = %pane.id,
                        "plugin pane placement `overlay` is not hosted yet (deferred); skipping entry",
                    );
                    continue;
                }
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
    fn overlay_placement_is_deferred_and_skipped() {
        let m = manifest(
            "p",
            vec![
                pane("ov", PluginPanePlacement::Overlay, vec!["x".to_owned()]),
                pane("s", PluginPanePlacement::Split, vec!["y".to_owned()]),
            ],
        );
        let entries = entries_from_manifests(std::slice::from_ref(&m));
        assert_eq!(entries.len(), 1, "overlay entry dropped, split kept");
        assert_eq!(entries[0].pane_id, "s");
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
        let FrameKind::SpawnTerminal {
            request_id,
            group,
            command,
            cwd,
            env,
            term,
            satellite,
            owner_terminal,
            agent_session,
        } = entry.spawn_frame(7)
        else {
            panic!("expected SpawnTerminal");
        };
        assert_eq!(request_id, 7);
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
    fn resolved_action_carries_plugin_and_pane_args() {
        let m = manifest(
            "p",
            vec![pane(
                "board",
                PluginPanePlacement::Tab,
                vec!["x".to_owned()],
            )],
        );
        let entry = &entries_from_manifests(std::slice::from_ref(&m))[0];
        let ra = entry.resolved_action();
        assert_eq!(ra.action, PLUGIN_PANE_NAME);
        assert_eq!(
            ra.args.get("plugin"),
            Some(&toml::Value::String("p".to_owned()))
        );
        assert_eq!(
            ra.args.get("pane"),
            Some(&toml::Value::String("board".to_owned()))
        );
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
