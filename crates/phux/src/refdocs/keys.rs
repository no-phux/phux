//! The generated default key bindings: the shipped prefix table grouped and
//! labelled exactly as the which-key popup shows it, plus the shipped global
//! bindings.

use std::fmt::Write as _;

use phux_config::{Action, Config};
use phux_tui::render::overlay::which_key::legend;

use super::Page;

/// Actions the popup collapses into one row of four directional keys.
const DIRECTIONAL: [&str; 2] = ["focus-direction", "resize-pane"];

/// One key as `<kbd>`, HTML- and table-escaped.
fn kbd(key: &str) -> String {
    let escaped = key
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "\\|");
    format!("<kbd>{escaped}</kbd>")
}

/// A legend key cell: the `0-9` tab-jump range reads as a range.
fn key_cell(key: &str) -> String {
    match key.split_once('-') {
        Some((first, last))
            if first.len() == 1
                && last.len() == 1
                && first.chars().all(|c| c.is_ascii_digit())
                && last.chars().all(|c| c.is_ascii_digit()) =>
        {
            format!("{}–{}", kbd(first), kbd(last))
        }
        _ => kbd(key),
    }
}

/// `split-pane direction=vertical`, for the global table.
fn action_cell(action: &Action) -> String {
    match action {
        Action::Bare(name) => format!("`{name}`"),
        Action::Parameterized(p) => {
            let args: Vec<String> = p
                .args
                .iter()
                .map(|(k, v)| match v {
                    toml::Value::String(s) => format!("{k}={s}"),
                    other => format!("{k}={other}"),
                })
                .collect();
            format!("`{}` `{}`", p.action, args.join(" "))
        }
    }
}

/// Render `docs/reference/keys.md` from the shipped defaults.
pub(crate) fn page() -> Page {
    #[allow(clippy::expect_used, reason = "the embedded defaults always parse")]
    let defaults: Config = phux_config::parse_str(
        phux_config::DEFAULT_CONFIG_TOML,
        std::path::Path::new("default.toml"),
    )
    .expect("embedded defaults parse");
    let kb = &defaults.keybindings;

    let mut body = format!(
        "Press the prefix, {prefix}, then one of the keys below. Hesitate \
         after the prefix and phux shows this same list in a popup. Every \
         key can be rebound or unbound in `[keybindings.prefix-table]`; see \
         [Key bindings](../keys.md). `phux config keys` prints the bindings \
         in effect, your changes included.\n\n\
         A capital letter means Shift: {upper_x} is <kbd>Shift</kbd>+<kbd>x</kbd>. \
         Where two keys share a row, either one works.\n",
        prefix = kbd(&kb.prefix),
        upper_x = kbd("X"),
    );

    let mut current = "";
    for row in legend(kb) {
        if row.group != current {
            current = row.group;
            let _ = write!(
                body,
                "\n## {current}\n\n| Keys | Does | Action |\n|---|---|---|\n"
            );
        }
        let keys: Vec<String> = row.keys.iter().map(|key| key_cell(key)).collect();
        // The popup's directional rows read "Focus"; spell out the keys' order.
        let label = if DIRECTIONAL.contains(&row.action.as_str()) && row.keys.len() == 4 {
            format!("{} left, down, up, right", row.label)
        } else {
            row.label.clone()
        };
        let _ = writeln!(
            body,
            "| {} | {} | `{}` |",
            keys.join(" "),
            label.replace('|', "\\|"),
            row.action
        );
    }

    body.push_str("\n## Without the prefix\n\n");
    if kb.global.is_empty() {
        body.push_str(
            "No global bindings ship, so every key without the prefix goes to \
             the program in the pane. Add your own under \
             `[keybindings.global]`.\n",
        );
    } else {
        body.push_str("| Keys | Action |\n|---|---|\n");
        for (key, action) in &kb.global {
            let _ = writeln!(body, "| {} | {} |", kbd(key), action_cell(action));
        }
    }

    Page {
        file: "keys.md",
        title: "Default key bindings",
        summary: "Every key phux binds out of the box, grouped as the which-key popup shows them.",
        lede: "Every key phux binds out of the box.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::{key_cell, page};

    #[test]
    fn every_shipped_prefix_binding_is_on_the_page() {
        let defaults = phux_config::parse_str(
            phux_config::DEFAULT_CONFIG_TOML,
            std::path::Path::new("default.toml"),
        )
        .expect("defaults parse");
        let page = page();
        for name in defaults
            .keybindings
            .prefix_table
            .values()
            .map(|action| match action {
                phux_config::Action::Bare(name) => name.clone(),
                phux_config::Action::Parameterized(p) => p.action.clone(),
            })
        {
            assert!(page.body.contains(&format!("`{name}`")), "missing {name}");
        }
    }

    #[test]
    fn keys_are_escaped_for_html_and_tables() {
        assert_eq!(key_cell("<"), "<kbd>&lt;</kbd>");
        assert_eq!(key_cell("|"), "<kbd>\\|</kbd>");
        assert_eq!(key_cell("0-9"), "<kbd>0</kbd>–<kbd>9</kbd>");
        assert_eq!(key_cell("-"), "<kbd>-</kbd>");
    }
}
