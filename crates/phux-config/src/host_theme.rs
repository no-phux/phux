//! Per-host themes: `[[remote]] theme` and `[host] theme` name the theme a
//! client wears while it views that machine, so a production box can look
//! unlike the rest. Unset everywhere, nothing changes.

use crate::theme::{FILE_KEY, NAME_KEY};
use crate::{Config, ThemeCfg};

impl Config {
    /// The theme `host` asks for: a `[[remote]]` name, or `None` for this
    /// machine (`[host] theme`). `None` keeps `[theme]`.
    #[must_use]
    pub fn host_theme(&self, host: Option<&str>) -> Option<&str> {
        let theme = match host {
            None => self.host.theme.as_deref(),
            Some(name) => self
                .remote
                .iter()
                .find(|r| r.name == name)?
                .theme
                .as_deref(),
        };
        theme.filter(|name| !name.trim().is_empty())
    }

    /// Whether any host names a theme; without one a client never switches.
    #[must_use]
    pub fn any_host_theme(&self) -> bool {
        self.host_theme(None).is_some()
            || self
                .remote
                .iter()
                .any(|r| self.host_theme(Some(&r.name)).is_some())
    }

    /// `[theme]` as worn while viewing `host`: its per-host theme selected,
    /// the slot overrides kept.
    #[must_use]
    pub fn theme_for_host(&self, host: Option<&str>) -> std::borrow::Cow<'_, ThemeCfg> {
        let Some(name) = self.host_theme(host) else {
            return std::borrow::Cow::Borrowed(&self.theme);
        };
        let mut slots = self.theme.slots.clone();
        slots.remove(FILE_KEY);
        slots.insert(NAME_KEY.to_owned(), name.to_owned());
        std::borrow::Cow::Owned(ThemeCfg { slots })
    }
}

#[cfg(test)]
mod tests {
    use crate::theme::ThemeSelection;
    use crate::{Config, RemoteConfigEntry};

    fn config() -> Config {
        let mut cfg = Config::default();
        cfg.theme
            .slots
            .insert("file".into(), "~/colors.toml".into());
        cfg.theme.slots.insert("accent".into(), "#ff9e64".into());
        let remote = |name: &str, theme: Option<&str>| RemoteConfigEntry {
            name: name.into(),
            theme: theme.map(Into::into),
            ..RemoteConfigEntry::default()
        };
        cfg.remote = vec![remote("prod", Some("red")), remote("dev", None)];
        cfg
    }

    #[test]
    fn a_themed_host_selects_its_theme_and_keeps_the_overrides() {
        let cfg = config();
        let prod = cfg.theme_for_host(Some("prod"));
        assert_eq!(prod.selection(), ThemeSelection::Name("red"));
        assert_eq!(
            prod.slots.get("accent").map(String::as_str),
            Some("#ff9e64")
        );
        assert!(cfg.any_host_theme());
    }

    #[test]
    fn an_unthemed_host_or_this_machine_keeps_the_global_theme() {
        let mut cfg = config();
        for host in [Some("dev"), Some("unknown"), None] {
            assert_eq!(*cfg.theme_for_host(host), cfg.theme, "{host:?}");
        }
        cfg.host.theme = Some("nord".into());
        assert_eq!(
            cfg.theme_for_host(None).selection(),
            ThemeSelection::Name("nord")
        );
        cfg.host.theme = Some(String::new());
        cfg.remote.clear();
        assert!(!cfg.any_host_theme());
    }
}
