//! The theme in your Ghostty config, read the way Ghostty reads it: the
//! `theme` file first, then the config and its `config-file` includes,
//! later lines winning. Only the colours are taken. A slot the config leaves
//! out comes from Catppuccin, so the terminal always has sixteen.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::{LATTE, MOCHA, THEMES, Theme, color};

/// The config's colours as a theme, read once per launch. `None` without a
/// config, or one that names no background and foreground.
pub fn ghostty() -> Option<&'static Theme> {
    static THEME: OnceLock<Option<Theme>> = OnceLock::new();
    THEME.get_or_init(read).as_ref()
}

fn read() -> Option<Theme> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| home.join(".config"));
    read_in(&home, &config_home)
}

/// `read` for an explicit home and XDG config directory.
fn read_in(home: &Path, config_home: &Path) -> Option<Theme> {
    let mac = home.join("Library/Application Support/com.mitchellh.ghostty");
    let main = [
        config_home.join("ghostty/config"),
        config_home.join("ghostty/config.ghostty"),
        mac.join("config"),
        mac.join("config.ghostty"),
    ]
    .into_iter()
    .find(|path| path.is_file())?;
    let own = super::read_config(&main)?;
    let mut texts = vec![own];
    texts.extend(includes(&main, &texts[0], home));
    let joined = texts.join("\n");
    let themed = theme_file(&joined, config_home).and_then(|path| super::read_config(&path));
    parse(
        &themed
            .into_iter()
            .chain(texts)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// `key = value` lines, comments and blanks skipped, values unquoted.
pub(super) fn lines(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        Some((key.trim(), value))
    })
}

/// `#rrggbb`, `rrggbb` or `#rgb`.
pub(super) fn hex(value: &str) -> Option<u32> {
    let body = value.trim().trim_start_matches('#');
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_hexdigit());
    match body.len() {
        6 if digits(body) => u32::from_str_radix(body, 16).ok(),
        3 if digits(body) => {
            let wide: String = body.chars().flat_map(|c| [c, c]).collect();
            u32::from_str_radix(&wide, 16).ok()
        }
        _ => None,
    }
}

/// The colours in `text` as a theme. Later lines win; a line that isn't a
/// colour leaves the earlier one standing.
pub fn parse(text: &str) -> Option<Theme> {
    let (mut background, mut foreground, mut cursor) = (None, None, None);
    let mut palette = [None; 16];
    for (key, value) in lines(text) {
        match key {
            "background" => background = hex(value).or(background),
            "foreground" => foreground = hex(value).or(foreground),
            "cursor-color" => cursor = hex(value).or(cursor),
            "palette" => {
                if let Some((slot, colour)) = value.split_once('=')
                    && let Ok(slot) = slot.trim().parse::<usize>()
                    && let Some(found) = palette.get_mut(slot)
                    && let Some(colour) = hex(colour)
                {
                    *found = Some(colour);
                }
            }
            _ => {}
        }
    }
    let (background, foreground) = (background?, foreground?);
    let dark = color(background).luminance() < 0.4;
    let mut ansi = THEMES[if dark { MOCHA } else { LATTE }].ansi;
    for (slot, found) in ansi.iter_mut().zip(palette) {
        if let Some(colour) = found {
            *slot = colour;
        }
    }
    Some(Theme {
        id: "ghostty",
        name: "Ghostty",
        dark,
        ansi,
        foreground,
        background,
        cursor: cursor.unwrap_or(foreground),
        // The blue slot: a cursor is often the foreground, which no wash reads on.
        accent: ansi[4],
    })
}

/// The texts of `config-file` includes, in order. Ghostty marks an optional
/// include with `?` outside the quotes.
fn includes(main: &Path, text: &str, home: &Path) -> Vec<String> {
    lines(text)
        .filter(|(key, _)| *key == "config-file")
        .filter_map(|(_, value)| {
            let value = value.trim_start_matches('?').trim().trim_matches('"');
            if value.is_empty() {
                return None;
            }
            let path = if Path::new(value).is_absolute() {
                PathBuf::from(value)
            } else if let Some(rest) = value.strip_prefix("~/") {
                home.join(rest)
            } else {
                main.parent().unwrap_or(Path::new(".")).join(value)
            };
            super::read_config(&path)
        })
        .collect()
}

/// The file the last `theme` line names: the dark one of a `dark:X,light:Y`
/// pair, by path or from the user's and Ghostty's own theme folders.
fn theme_file(text: &str, config_home: &Path) -> Option<PathBuf> {
    let value = lines(text).filter(|(key, _)| *key == "theme").last()?.1;
    let name = if value.contains(':') {
        value
            .split(',')
            .find_map(|part| part.trim().strip_prefix("dark:"))?
    } else {
        value
    }
    .trim();
    if name.is_empty() {
        return None;
    }
    if Path::new(name).is_absolute() {
        return Some(PathBuf::from(name));
    }
    [
        config_home.join("ghostty/themes").join(name),
        Path::new("/Applications/Ghostty.app/Contents/Resources/ghostty/themes").join(name),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r##"
# a comment
background = #1a1b26
foreground = "#c0caf5"
cursor-color = fff
palette = 1=#f7768e
palette = 4=#7aa2f7
palette = 99=#000000
font-size = 14
"##;

    #[test]
    fn colours_come_through_and_the_rest_is_catppuccin() {
        let theme = parse(CONFIG).expect("a theme");
        assert_eq!(theme.background, 0x1a1b26);
        assert_eq!(theme.foreground, 0xc0caf5);
        assert_eq!(theme.cursor, 0xffffff);
        assert_eq!(theme.ansi[1], 0xf7768e);
        assert_eq!(theme.ansi[2], THEMES[MOCHA].ansi[2]);
        assert_eq!(theme.accent, 0x7aa2f7);
        assert!(theme.dark);
        assert_eq!(theme.id, "ghostty");
    }

    #[test]
    fn a_light_background_reads_as_light_and_needs_both_colours() {
        let light = parse("background = #fdf6e3\nforeground = #586e75").expect("a theme");
        assert!(!light.dark);
        assert_eq!(light.ansi, THEMES[LATTE].ansi);
        assert_eq!(light.cursor, light.foreground);
        assert_eq!(parse("background = #000000"), None);
        assert_eq!(parse("background = #000000\nforeground = blue"), None);
    }

    #[test]
    fn later_lines_win_and_a_bad_one_keeps_the_earlier() {
        let theme =
            parse("background=#000\nforeground=#fff\nbackground = #123456\nbackground = nope")
                .expect("a theme");
        assert_eq!(theme.background, 0x123456);
    }

    #[test]
    fn the_theme_file_is_read_first_and_includes_after() {
        let dir = std::env::temp_dir().join(format!("phux-rune-ghostty-{}", std::process::id()));
        let config_home = dir.join("config");
        std::fs::create_dir_all(config_home.join("ghostty/themes")).unwrap();
        std::fs::write(
            config_home.join("ghostty/themes/Mine"),
            "background = #111111\nforeground = #eeeeee\npalette = 4=#123456",
        )
        .unwrap();
        std::fs::write(config_home.join("ghostty/extra"), "foreground = #dddddd").unwrap();
        std::fs::write(
            config_home.join("ghostty/config"),
            "theme = dark:Mine,light:Other\nconfig-file = ?\"extra\"\nbackground = #222222",
        )
        .unwrap();
        let theme = read_in(&dir, &config_home).expect("a theme");
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(theme.background, 0x222222);
        assert_eq!(theme.foreground, 0xdddddd);
        assert_eq!(theme.accent, 0x123456);
    }

    #[test]
    fn no_config_is_no_theme() {
        let dir = std::env::temp_dir().join("phux-rune-ghostty-none");
        assert_eq!(read_in(&dir, &dir), None);
    }
}
