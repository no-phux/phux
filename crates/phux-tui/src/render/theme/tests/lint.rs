//! Guards over the theme sources: the shipped config and literal colours.

use super::*;

/// The shipped config's commented `[theme]` block lists every slot with
/// the house look's value, so `phux config init` never shows a stale one.
#[test]
fn the_shipped_config_lists_every_slot_at_its_house_value() {
    let shipped = include_str!("../../../../../phux-config/src/default/status-and-chrome.toml");
    let theme = Theme::default();
    for spec in SLOT_SPECS {
        let key = spec.leaf();
        let line = shipped
            .lines()
            .find(|l| {
                l.strip_prefix("# ")
                    .and_then(|rest| rest.strip_prefix(key))
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            })
            .unwrap_or_else(|| panic!("the shipped config lists `{key}`"));
        let value = line.split('"').nth(1).expect("a quoted value");
        assert_eq!(
            Some(value.to_owned()),
            theme.slot(key).map(color_to_string),
            "{key}"
        );
    }
}

/// Spec §5.1: no chrome code paints a literal colour. Every `Color::Rgb`
/// or `Color::Indexed` under `src/render` lives in the theme or the SGR
/// emitter; test code is exempt.
#[test]
fn no_literal_colors() {
    let root = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo test sets CARGO_MANIFEST_DIR"),
    )
    .join("src/render");
    let mut offenders = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src/render") {
            let path = entry.expect("dir entry").path();
            let rel = path.strip_prefix(&root).expect("under root").to_path_buf();
            if path.is_dir() {
                if rel != std::path::Path::new("theme") && !rel.ends_with("tests") {
                    stack.push(path);
                }
                continue;
            }
            let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let exempt = rel == std::path::Path::new("sgr.rs")
                || name == "tests.rs"
                || name.ends_with("_tests.rs")
                || path.extension().is_none_or(|ext| ext != "rs");
            if exempt {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source");
            for (line_no, line) in production_lines(&text) {
                if line.contains("Color::Rgb(") || line.contains("Color::Indexed(") {
                    offenders.push(format!("{}:{line_no}: {}", rel.display(), line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "literal colours outside render/theme and render/sgr.rs:\n{}",
        offenders.join("\n")
    );
}

/// The lines of `text` before its inline `#[cfg(test)] mod … {` block.
fn production_lines(text: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if *line == "#[cfg(test)]" {
            let next = lines[i + 1..]
                .iter()
                .find(|l| !l.starts_with("#["))
                .copied()
                .unwrap_or("");
            if next.starts_with("mod ") && next.trim_end().ends_with('{') {
                break;
            }
        }
        out.push((i + 1, *line));
    }
    out
}
