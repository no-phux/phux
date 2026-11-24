//! `[[links]]` handlers: which configured handler takes a clicked link, and
//! the argv and environment it runs with. Handlers come only from the user's
//! config; the link and pane are data, substituted into argv elements and
//! passed as `PHUX_LINK_*` variables, never handed to a shell.

use std::path::{Path, PathBuf};

use phux_config::loader as config_loader;
use phux_config::plugin::{self, PluginLinkRunOn, PluginManifestLinkHandler, platforms_allow};

use crate::PluginActionError;

/// One platform-compatible link handler from an enabled plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredLink {
    /// Manifest id of the plugin.
    pub plugin: String,
    /// The plugin's directory: the working directory of a client run.
    pub plugin_root: PathBuf,
    /// The handler as declared.
    pub handler: PluginManifestLinkHandler,
}

/// Where a click happened: the pane that printed the link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkContext {
    /// The link as clicked: a URL or a path, maybe with `:line[:col]`.
    pub url: String,
    /// The pane, as the client names it.
    pub pane: String,
    /// The machine that runs the pane; empty for this one.
    pub host: String,
    /// The pane's working directory, when known.
    pub cwd: String,
}

/// What a matched handler runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkCommand {
    /// Argv with the placeholders filled in.
    pub argv: Vec<String>,
    /// `PHUX_LINK_*` and plugin variables, added to the environment.
    pub env: Vec<(String, String)>,
    /// Where it runs.
    pub run_on: PluginLinkRunOn,
}

/// Programs that would read a filled-in argv element as code.
const SHELLS: [&str; 10] = [
    "sh", "bash", "zsh", "dash", "fish", "ksh", "csh", "tcsh", "nu", "pwsh",
];
const PLACEHOLDERS: [&str; 6] = ["{url}", "{path}", "{line}", "{pane}", "{host}", "{cwd}"];

/// Every platform-compatible handler in the local configuration, in
/// `[[plugins]]` then manifest order.
///
/// # Errors
///
/// When the config can't be loaded; a broken manifest is skipped.
pub fn configured_links(config_path: &Path) -> Result<Vec<ConfiguredLink>, PluginActionError> {
    let cfg = config_loader::load_from(config_path)?;
    let manifests = plugin::load_enabled_manifests(config_path, &cfg.plugins);
    Ok(manifests
        .into_iter()
        .filter(|manifest| platforms_allow(manifest.platforms.as_deref()))
        .flat_map(|manifest| {
            let (id, root) = (manifest.id, manifest.plugin_root);
            manifest
                .links
                .into_iter()
                .filter(|handler| platforms_allow(handler.platforms.as_deref()))
                .map(move |handler| ConfiguredLink {
                    plugin: id.clone(),
                    plugin_root: root.clone(),
                    handler,
                })
        })
        .collect())
}

/// The first handler that takes `url`, or `None` for the default open.
/// A link that could pass as an option (`-x`) or holds a control character
/// takes none.
#[must_use]
pub fn handler_for<'a>(links: &'a [ConfiguredLink], url: &str) -> Option<&'a ConfiguredLink> {
    if url.is_empty() || url.starts_with('-') || url.chars().any(char::is_control) {
        return None;
    }
    links.iter().find(|link| matches(&link.handler, url))
}

fn matches(handler: &PluginManifestLinkHandler, url: &str) -> bool {
    let scheme = url.split_once(':').map(|(scheme, _)| scheme);
    handler
        .schemes
        .iter()
        .any(|want| scheme.is_some_and(|scheme| scheme.eq_ignore_ascii_case(want)))
        || handler.patterns.iter().any(|pattern| glob(pattern, url))
}

/// Whole-string match where `*` stands for any run of characters.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// A path without its `:line[:col]` suffix, and the line. A URL has none.
fn path_and_line(url: &str) -> (&str, &str) {
    if url.contains("://") {
        return (url, "");
    }
    let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (mut path, mut line) = (url, "");
    for _ in 0..2 {
        match path.rsplit_once(':') {
            Some((head, tail)) if number(tail) => (path, line) = (head, tail),
            _ => break,
        }
    }
    (path, line)
}

impl ConfiguredLink {
    /// The handler's argv and environment for a click in `context`.
    ///
    /// # Errors
    ///
    /// When the command is a shell and an argument holds a placeholder: a
    /// shell would run a crafted link as code. It reads `$PHUX_LINK_URL`.
    pub fn command(&self, context: &LinkContext) -> Result<LinkCommand, String> {
        let command = &self.handler.command;
        let program = command.first().map(Path::new).and_then(Path::file_name);
        let shell = program.is_some_and(|name| SHELLS.iter().any(|shell| name == *shell));
        if shell
            && command
                .iter()
                .any(|arg| PLACEHOLDERS.iter().any(|p| arg.contains(p)))
        {
            return Err(format!(
                "Link handler {}/{} runs a shell, so it must read the link from \
                 $PHUX_LINK_URL rather than a {{placeholder}}.",
                self.plugin, self.handler.id
            ));
        }
        let (path, line) = path_and_line(&context.url);
        let values = [
            context.url.as_str(),
            path,
            line,
            context.pane.as_str(),
            context.host.as_str(),
            context.cwd.as_str(),
        ];
        let fill = |arg: &String| {
            PLACEHOLDERS
                .iter()
                .zip(values)
                .fold(arg.clone(), |arg, (placeholder, value)| {
                    arg.replace(placeholder, value)
                })
        };
        let mut argv: Vec<String> = command.iter().map(fill).collect();
        // The plugin's directory exists only on this machine.
        if self.handler.run_on == PluginLinkRunOn::Client {
            argv = crate::executable_argv(&self.plugin_root, &argv);
        }
        let env = [
            ("PHUX_LINK_URL", context.url.as_str()),
            ("PHUX_LINK_PATH", path),
            ("PHUX_LINK_LINE", line),
            ("PHUX_LINK_PANE", context.pane.as_str()),
            ("PHUX_LINK_HOST", context.host.as_str()),
            ("PHUX_LINK_CWD", context.cwd.as_str()),
            ("PHUX_PLUGIN_ID", self.plugin.as_str()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        Ok(LinkCommand {
            argv,
            env,
            run_on: self.handler.run_on,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(patterns: &[&str], schemes: &[&str], command: &[&str]) -> ConfiguredLink {
        let strings = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect();
        ConfiguredLink {
            plugin: "dev".to_owned(),
            plugin_root: PathBuf::from("/plugins/dev"),
            handler: PluginManifestLinkHandler {
                id: "edit".to_owned(),
                title: "Edit".to_owned(),
                description: None,
                contexts: Vec::new(),
                schemes: strings(schemes),
                patterns: strings(patterns),
                platforms: None,
                run_on: PluginLinkRunOn::PaneHost,
                command: strings(command),
            },
        }
    }

    #[test]
    fn the_first_matching_handler_takes_the_link_and_none_matches_otherwise() {
        let links = [
            link(&["*.rs:*"], &[], &["nvim"]),
            link(&[], &["HTTPS"], &["browser"]),
            link(&["*"], &[], &["catch-all"]),
        ];
        let first = |url| handler_for(&links, url).map(|l| l.handler.command[0].as_str());
        assert_eq!(first("src/main.rs:12:4"), Some("nvim"));
        assert_eq!(first("https://phux.sh"), Some("browser"));
        assert_eq!(first("notes.txt"), Some("catch-all"));
        assert_eq!(first("-oProxyCommand=x"), None, "an option");
        assert_eq!(first("a\nb"), None, "a control character");
        assert_eq!(handler_for(&links[..1], "src/main.rs"), None);
        assert_eq!(handler_for(&[], "src/main.rs:1"), None, "no config");
    }

    #[test]
    fn globs_match_the_whole_link() {
        assert!(glob("https://linear.app/*", "https://linear.app/x/1"));
        assert!(glob("*.rs:*", "a.rs:1") && glob("**", ""));
        assert!(!glob("*.rs", "a.rs:1") && !glob("https://linear.app/*", "http://x"));
        assert!(glob("a*b*c", "aXbYbZc") && !glob("a*b*c", "aXbY"));
    }

    #[test]
    fn placeholders_fill_argv_elements_and_the_environment() {
        let handler = link(&["*"], &[], &["nvim", "+{line}", "{path}", "--host={host}"]);
        let context = LinkContext {
            url: "src/a b.rs:12:4".to_owned(),
            pane: "@3".to_owned(),
            host: "box".to_owned(),
            cwd: "/w".to_owned(),
        };
        let run = handler.command(&context).expect("a command");
        assert_eq!(run.argv, ["nvim", "+12", "src/a b.rs", "--host=box"]);
        assert_eq!(run.run_on, PluginLinkRunOn::PaneHost);
        let var = |key: &str| {
            run.env
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(var("PHUX_LINK_URL"), Some("src/a b.rs:12:4"));
        assert_eq!(var("PHUX_LINK_CWD"), Some("/w"));
        assert_eq!(
            path_and_line("https://host:8080/x"),
            ("https://host:8080/x", "")
        );
        assert_eq!(path_and_line("a.rs"), ("a.rs", ""));
    }

    #[test]
    fn a_shell_command_may_not_take_placeholders() {
        let unsafe_handler = link(&["*"], &[], &["/bin/sh", "-c", "nvim {url}"]);
        assert!(unsafe_handler.command(&LinkContext::default()).is_err());
        let safe = link(&["*"], &[], &["sh", "-c", "nvim \"$PHUX_LINK_URL\""]);
        assert!(safe.command(&LinkContext::default()).is_ok());
    }

    #[test]
    fn a_client_run_finds_a_relative_program_in_the_plugin() {
        let mut handler = link(&["*"], &[], &["bin/open", "{url}"]);
        handler.handler.run_on = PluginLinkRunOn::Client;
        let run = handler.command(&LinkContext::default()).expect("a command");
        assert_eq!(run.argv[0], "/plugins/dev/bin/open");
    }
}
