//! Omarchy's current theme (<https://omarchy.org>). Omarchy writes a Ghostty
//! config for every theme, whatever terminal you use, so its colours come
//! through the Ghostty reader; the accent is the one its `colors.toml`
//! names. Omarchy swaps these files on every theme change, so a worker reads
//! them again while Rune runs.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rune::Waker;

use super::Theme;
use super::ghostty::{hex, lines, parse};

/// The theme as last read: `None` until the first read, then the theme or
/// `None` off Omarchy.
static CURRENT: Mutex<Option<Option<&'static Theme>>> = Mutex::new(None);

/// How often the worker looks for a new Omarchy theme.
const CHECK_EVERY: Duration = Duration::from_secs(2);

/// Omarchy's current theme, or `None` off Omarchy.
pub fn omarchy() -> Option<&'static Theme> {
    let mut current = CURRENT.lock().unwrap_or_else(PoisonError::into_inner);
    *current.get_or_insert_with(|| dir().and_then(|dir| read_in(&dir)).map(leak))
}

/// Read Omarchy's theme again. True when its colours changed.
fn refresh() -> bool {
    let fresh = dir().and_then(|dir| read_in(&dir));
    let mut current = CURRENT.lock().unwrap_or_else(PoisonError::into_inner);
    if current.flatten().copied() == fresh {
        return false;
    }
    *current = Some(fresh.map(leak));
    true
}

/// Wake Rune whenever Omarchy's theme changes. Nothing runs off Omarchy.
pub fn watch(waker: Waker) {
    if !dir().is_some_and(|dir| dir.is_dir()) {
        return;
    }
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(CHECK_EVERY);
            if refresh() {
                waker();
            }
        }
    });
}

// ponytail: each Omarchy theme change leaks one Theme (~100 bytes) so
// settings keep their `&'static Theme`; owned themes in Settings if anyone
// ever switches thousands of times in one run.
fn leak(theme: Theme) -> &'static Theme {
    Box::leak(Box::new(theme))
}

/// Where Omarchy keeps its current theme. It spells the path from `$HOME`.
fn dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(home.join(".local/state/omarchy/current/theme"))
}

/// The theme in `dir`. `None` without its Ghostty config.
fn read_in(dir: &Path) -> Option<Theme> {
    let theme = parse(&std::fs::read_to_string(dir.join("ghostty.conf")).ok()?)?;
    let accent = std::fs::read_to_string(dir.join("colors.toml"))
        .ok()
        .and_then(|colors| {
            lines(&colors)
                .filter(|(key, _)| *key == "accent")
                .find_map(|(_, value)| hex(value))
        })
        .unwrap_or(theme.accent);
    Some(Theme {
        id: "omarchy",
        name: "Omarchy",
        accent,
        ..theme
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_from_the_ghostty_config_and_the_accent_from_colors_toml() {
        let dir = std::env::temp_dir().join(format!("phux-rune-omarchy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ghostty.conf"),
            "background = #1a1b26\nforeground = #a9b1d6\npalette = 4=#7aa2f7\n",
        )
        .unwrap();
        let blue = read_in(&dir).expect("a theme");
        std::fs::write(
            dir.join("colors.toml"),
            "mode = \"dark\"\naccent = \"#ff9e64\"\n",
        )
        .unwrap();
        let theme = read_in(&dir).expect("a theme");
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((theme.id, theme.name), ("omarchy", "Omarchy"));
        assert_eq!((theme.background, theme.foreground), (0x1a1b26, 0xa9b1d6));
        assert!(theme.dark);
        assert_eq!(theme.accent, 0xff9e64);
        assert_eq!(blue.accent, 0x7aa2f7, "without colors.toml, the blue slot");
    }

    #[test]
    fn no_omarchy_is_no_theme() {
        assert_eq!(
            read_in(&std::env::temp_dir().join("phux-rune-omarchy-none")),
            None
        );
    }
}
