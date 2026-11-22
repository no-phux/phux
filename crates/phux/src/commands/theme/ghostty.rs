//! A Ghostty theme file as `colors.toml` text, so `phux theme install` takes
//! one. Ghostty's `palette = N=COLOUR` lines become the `colorN` keys the
//! theme loader already reads; nothing here decides how a theme resolves.

use std::fmt::Write as _;

/// `text` as `colors.toml`, or why it is not a Ghostty theme.
pub(super) fn colors_toml(text: &str) -> Result<String, String> {
    let mut out = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let (name, colour) = match key {
            "palette" => {
                let Some((index, colour)) = value.split_once('=') else {
                    continue;
                };
                let Ok(index) = index.trim().parse::<u8>() else {
                    continue;
                };
                (format!("color{index}"), colour.trim())
            }
            "background" | "foreground" => (key.to_owned(), value),
            "selection-background" => ("selection".to_owned(), value),
            _ => continue,
        };
        // Ghostty takes `#rrggbb` and bare `rrggbb`; a named colour is skipped.
        let hex = colour.trim_start_matches('#');
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let _ = writeln!(out, "{name} = \"#{hex}\"");
        }
    }
    if out.contains("background = ") && out.contains("color1 = ") {
        Ok(out)
    } else {
        Err("not a Ghostty theme: it needs a `background` and `palette` lines".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::colors_toml;

    #[test]
    fn a_ghostty_theme_loads_as_a_phux_theme() {
        let mut text = String::from("# a comment\nbackground = fafafa\nforeground = #383a42\n");
        text.push_str("cursor-color = #526eff\nselection-background = e5e5e6\nfont-size = 14\n");
        for (index, colour) in [
            "000000", "e45649", "50a14f", "c18401", "4078f2", "a626a4", "0184bc",
        ]
        .iter()
        .enumerate()
        {
            let _ = writeln!(text, "palette = {index}=#{colour}");
        }
        let toml = colors_toml(&text).expect("converts");
        assert!(toml.contains("color1 = \"#e45649\""), "{toml}");
        assert!(toml.contains("selection = \"#e5e5e6\""), "{toml}");
        assert!(!toml.contains("font"), "{toml}");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("colors.toml"), toml).expect("write");
        let theme = phux_config::theme::load_dir(dir.path()).expect("loads");
        assert_eq!(theme.mode, phux_config::theme::ThemeMode::Light);

        assert!(colors_toml("font-size = 14\n").is_err());
    }
}
