//! `phux omarchy keys`: the key bindings in effect, one `KEYS → what it
//! does` line each, the shape Omarchy's keybinding menus feed
//! `omarchy-menu-select`. Labels are the which-key popup's, so the menu
//! and the popup never disagree.

use std::process::ExitCode;

use phux_config::KeybindingsCfg;
use phux_config::loader as config_loader;
use phux_tui::attach::plugin_actions;
use phux_tui::render::overlay::which_key::legend;

pub(super) fn print() -> Result<ExitCode, String> {
    let cfg = config_loader::load().map_err(|err| err.to_string())?;
    let manifests = if cfg.plugins.is_empty() {
        Vec::new()
    } else {
        phux_config::plugin::load_enabled_manifests(&config_loader::config_path(), &cfg.plugins)
    };
    let mut keybindings = cfg.keybindings;
    plugin_actions::merge_plugin_bindings(
        &mut keybindings,
        &plugin_actions::entries_from_manifests(&manifests),
    );
    for line in lines(&keybindings) {
        outln!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Keys that need no prefix first, as the ones a hand reaches for most,
/// then the prefix table under its prefix.
fn lines(keybindings: &KeybindingsCfg) -> Vec<String> {
    // The legend groups and labels one table: hand it the global table as
    // if it were the prefix table, then print those keys bare.
    let globals = KeybindingsCfg {
        prefix_table: keybindings.global.clone(),
        ..keybindings.clone()
    };
    let rows = legend(&globals)
        .into_iter()
        .map(|row| (row.keys.join(" / "), row.label))
        .chain(legend(keybindings).into_iter().map(|row| {
            let keys: Vec<String> = row
                .keys
                .iter()
                .map(|key| format!("{} {key}", keybindings.prefix))
                .collect();
            (keys.join(" / "), row.label)
        }));
    let mut lines = vec![format!("{:<32} → {}", "PREFIX", keybindings.prefix)];
    lines.extend(rows.map(|(keys, label)| format!("{keys:<32} → {label}")));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_global_keys_bare_and_prefix_keys_after_the_prefix() {
        let cfg = phux_config::parse_with_defaults(
            super::super::LAYER,
            std::path::Path::new("/tmp/omarchy.toml"),
        )
        .expect("layer parses");
        let lines = lines(&cfg.keybindings);
        assert!(lines[0].starts_with("PREFIX") && lines[0].ends_with("C-Space"));
        assert!(
            lines.iter().any(|line| line.starts_with("M-Enter ")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|line| line.starts_with("C-Space q ")
                || line.contains(" / C-Space q ")
                || line.contains("C-Space q /")),
            "{lines:#?}"
        );
        assert!(lines.iter().all(|line| line.contains(" → ")));
    }
}
