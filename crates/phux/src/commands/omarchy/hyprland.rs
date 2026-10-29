//! The Hyprland side of `phux omarchy`: which files hold the user's binds
//! and autostart, and the lines phux puts in them, in Lua (Omarchy's
//! Hyprland config today) or hyprlang (before it moved to Lua).

use std::path::{Path, PathBuf};

pub(super) struct Hyprland {
    lua: bool,
    pub(super) bindings: PathBuf,
    pub(super) autostart: PathBuf,
}

impl Hyprland {
    /// The files under `hypr` (`~/.config/hypr`), in the dialect its main
    /// config is written in. `None` without a Hyprland config.
    pub(super) fn detect(hypr: &Path) -> Option<Self> {
        let lua = if hypr.join("hyprland.lua").is_file() {
            true
        } else if hypr.join("hyprland.conf").is_file() {
            false
        } else {
            return None;
        };
        let ext = if lua { "lua" } else { "conf" };
        Some(Self {
            lua,
            bindings: hypr.join(format!("bindings.{ext}")),
            autostart: hypr.join(format!("autostart.{ext}")),
        })
    }

    /// What starts a line comment, for the marked blocks.
    pub(super) const fn comment(&self) -> &'static str {
        if self.lua { "--" } else { "#" }
    }

    /// Super+Ctrl+Return opens phux, Super+Ctrl+K lists its keys. Omarchy
    /// gives these two keys to its preinstalled multiplexer.
    pub(super) fn bindings_block(&self, phux: &str) -> String {
        let menu = format!(
            "sh -c '{phux} omarchy keys | omarchy-menu-select \"Phux keybindings\" -- --width 800'"
        );
        if self.lua {
            format!(
                "o.rebind(\"SUPER + CTRL + RETURN\", \"Phux\", \"omarchy-launch-terminal {phux}\")\n\
                 o.rebind(\"SUPER + CTRL + K\", \"Phux keybindings\", [[{menu}]])"
            )
        } else {
            format!(
                "unbind = SUPER CTRL, RETURN\n\
                 bindd = SUPER CTRL, RETURN, Phux, exec, uwsm-app -- xdg-terminal-exec {phux}\n\
                 unbind = SUPER CTRL, K\n\
                 bindd = SUPER CTRL, K, Phux keybindings, exec, {menu}"
            )
        }
    }

    /// Start the server with the graphical session, so its panes inherit
    /// `WAYLAND_DISPLAY` (the clipboard) even when the first attach comes
    /// over SSH.
    pub(super) fn autostart_block(&self, phux: &str) -> String {
        if self.lua {
            format!("o.launch_on_start(\"{phux} server --ensure\")")
        } else {
            format!("exec-once = uwsm-app -- {phux} server --ensure")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hyprland(lua: bool) -> Hyprland {
        Hyprland {
            lua,
            bindings: PathBuf::new(),
            autostart: PathBuf::new(),
        }
    }

    #[test]
    fn desktop_blocks_speak_each_hyprland_dialect() {
        let lua = hyprland(true);
        let binds = lua.bindings_block("/usr/bin/phux");
        assert!(binds.contains(
            "o.rebind(\"SUPER + CTRL + RETURN\", \"Phux\", \"omarchy-launch-terminal /usr/bin/phux\")"
        ));
        assert!(binds.contains("[[sh -c '/usr/bin/phux omarchy keys | omarchy-menu-select"));
        assert_eq!(
            lua.autostart_block("phux"),
            "o.launch_on_start(\"phux server --ensure\")"
        );
        let conf = hyprland(false);
        assert!(conf.bindings_block("phux").contains(
            "bindd = SUPER CTRL, RETURN, Phux, exec, uwsm-app -- xdg-terminal-exec phux"
        ));
        assert_eq!(
            conf.autostart_block("phux"),
            "exec-once = uwsm-app -- phux server --ensure"
        );
    }

    #[test]
    fn the_dialect_follows_the_main_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(Hyprland::detect(dir.path()).is_none());
        std::fs::write(dir.path().join("hyprland.conf"), "").expect("write");
        let conf = Hyprland::detect(dir.path()).expect("conf");
        assert_eq!(conf.comment(), "#");
        assert!(conf.bindings.ends_with("bindings.conf"));
        std::fs::write(dir.path().join("hyprland.lua"), "").expect("write");
        let lua = Hyprland::detect(dir.path()).expect("lua");
        assert_eq!(lua.comment(), "--");
        assert!(lua.autostart.ends_with("autostart.lua"));
    }
}
