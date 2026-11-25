//! A ⌘-click on a link a configured `[[links]]` handler takes runs that
//! handler instead of the default open: on this machine (`run_on =
//! "client"`), or in a split beside the pane on the machine that runs it
//! (`"pane-host"`).

use phux_client_core::layout::SplitDir;
use phux_client_runtime::control::SpawnRequest;
use phux_config::plugin::PluginLinkRunOn;
use phux_plugin::{ConfiguredLink, LinkContext};
use phux_protocol::ids::ResourceId;

use super::{Desk, NoticeKind};
use crate::links;

impl Desk {
    /// The handler that takes `url`, if any.
    pub(super) fn link_handler(&self, url: &str) -> Option<&ConfiguredLink> {
        phux_plugin::handler_for(&self.plugin_actions.links, url)
    }

    /// Run the handler that takes `url`. False: none does, so the link
    /// opens as it always has.
    pub(super) fn run_link_handler(&mut self, terminal: &ResourceId, url: &str) -> bool {
        let Some(handler) = self.link_handler(url).cloned() else {
            return false;
        };
        let cwd = self
            .connection
            .topology
            .as_ref()
            .and_then(|t| t.pane(terminal)?.cwd.clone())
            .unwrap_or_default();
        let context = LinkContext {
            url: url.to_owned(),
            pane: terminal.to_string(),
            host: terminal
                .host()
                .map(ToString::to_string)
                .or_else(|| self.machines.current.clone())
                .unwrap_or_default(),
            cwd,
        };
        let run = match handler.command(&context) {
            Ok(run) => run,
            Err(why) => {
                self.say(NoticeKind::Error, why);
                return true;
            }
        };
        match run.run_on {
            PluginLinkRunOn::Client => {
                let mut command = std::process::Command::new(&run.argv[0]);
                command.args(&run.argv[1..]).envs(run.env);
                // Another machine's directory could merely share the path.
                let here = std::path::Path::new(&context.cwd);
                if !self.remote(terminal) && here.is_dir() {
                    command.current_dir(here);
                } else {
                    command.current_dir(&handler.plugin_root);
                }
                if let Err(error) = links::spawn_opener(command, url, self.opener_failed()) {
                    self.say(NoticeKind::Error, format!("Couldn't open {url}: {error}"));
                }
            }
            // A split spawns on this window's server, which is not where a
            // federated satellite's pane runs.
            PluginLinkRunOn::PaneHost if terminal.host().is_some() => self.say(
                NoticeKind::Error,
                format!(
                    "Link handler {}/{} runs on the pane's machine; open a window on {} to use it.",
                    handler.plugin, handler.handler.id, context.host
                ),
            ),
            PluginLinkRunOn::PaneHost => {
                let spawn = SpawnRequest {
                    command: Some(run.argv),
                    cwd: (!context.cwd.is_empty()).then_some(context.cwd),
                    env: Some(run.env),
                    session_id: None,
                    initial_size: None,
                };
                self.split_with(terminal.clone(), SplitDir::Horizontal, spawn);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phux_config::plugin::PluginManifestLinkHandler;

    use super::*;
    use crate::launch::Resolved;

    fn desk() -> Desk {
        Desk::with_preferences(
            &Resolved::offline("link-handlers"),
            None,
            rune::prefs::Prefs::memory(),
        )
    }

    #[test]
    fn no_handler_leaves_the_click_alone_and_a_satellite_pane_host_run_is_refused() {
        let mut desk = desk();
        let satellite = ResourceId::satellite("box:2222", 1);
        assert!(!desk.run_link_handler(&satellite, "src/main.rs:3"));
        desk.plugin_actions.links.push(ConfiguredLink {
            plugin: "dev".to_owned(),
            plugin_root: PathBuf::from("/nowhere"),
            handler: PluginManifestLinkHandler {
                id: "edit".to_owned(),
                title: "Edit".to_owned(),
                description: None,
                contexts: Vec::new(),
                schemes: Vec::new(),
                patterns: vec!["*.rs:*".to_owned()],
                platforms: None,
                run_on: PluginLinkRunOn::PaneHost,
                command: vec!["nvim".to_owned(), "+{line}".to_owned(), "{path}".to_owned()],
            },
        });
        assert!(desk.run_link_handler(&satellite, "src/main.rs:3"));
        let notice = desk.notice.as_ref().map(|notice| notice.text.clone());
        assert!(notice.is_some_and(|text| text.contains("box:2222")));
        assert!(!desk.run_link_handler(&satellite, "https://phux.sh"));
    }
}
