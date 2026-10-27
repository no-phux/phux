use std::path::{Path, PathBuf};

use super::{HOOK, LAYER, layer_extends, layer_text};

#[test]
fn the_layer_is_valid_config_and_binds_omarchy_keys() {
    let path = Path::new("/tmp/omarchy.toml");
    let cfg = phux_config::parse_with_defaults(LAYER, path).expect("layer parses");
    let report = phux_config::check::check(LAYER, path).expect("check runs");
    assert!(
        report.findings.is_empty(),
        "{:?}",
        report.findings.first().map(|f| &f.message)
    );
    assert_eq!(cfg.keybindings.prefix, "C-Space");
    let (_, skipped) = phux_config::keybind::Resolver::new_lenient(&cfg.keybindings);
    assert!(skipped.is_empty(), "every key resolves: {skipped:?}");
    for key in ["M-Enter", "M-S-Enter", "C-M-Left", "M-1", "M-Up"] {
        assert!(cfg.keybindings.global.contains_key(key), "{key}");
    }
}

#[test]
fn the_layer_extends_the_distro_it_replaced() {
    let text = layer_text(Some(Path::new("/d/crew/crew.toml")));
    assert_eq!(
        layer_extends(&text),
        Some(PathBuf::from("/d/crew/crew.toml"))
    );
    assert_eq!(layer_extends(&layer_text(None)), None);
}

#[test]
fn the_hook_quotes_the_binary_and_tolerates_a_missing_server() {
    let hook = HOOK.replace(
        "{PHUX}",
        &crate::commands::sh_quote("/home/me/.local/bin/phux"),
    );
    assert!(hook.starts_with("#!/bin/bash\n"));
    assert!(hook.contains("\nphux='/home/me/.local/bin/phux'\n"));
    assert!(hook.ends_with("config reload >/dev/null 2>&1 || true\n"));
}
