//! Manifest checks shared with `phux plugin validate`.
//!
//! `load_plugin_manifest` rejects a document the loader will not accept,
//! including a `min_phux_version` newer than this phux and a theme directory
//! with no `colors.toml`. `dead_events` rejects an `[[events]]` hook whose
//! `on` is not a hook event: the loader keeps those, and `phux plugin validate`
//! reports them.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use phux_config::plugin::{self, PluginManifest};
use phux_config::theme::COLORS_FILE;

use crate::Error;

#[derive(Debug)]
pub(crate) struct Facts {
    pub id: String,
    pub name: String,
    pub description: String,
    pub min_phux_version: String,
    pub kinds: Vec<String>,
}

pub(crate) struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl TempDir {
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

pub(crate) fn scratch() -> Result<TempDir, Error> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "phux-plugin-index-{}-{nanos}-{n}",
        std::process::id()
    ));
    fs::create_dir(&path)
        .map_err(|err| Error::new(format!("could not create {}: {err}", path.display())))?;
    Ok(TempDir(path))
}

/// Validate a manifest already on disk.
///
/// The `Err` string is a skip reason. Missing files are the caller's concern.
pub(crate) fn judge(manifest_path: &Path, logical_root: &str) -> Result<Facts, String> {
    let manifest = match plugin::load_plugin_manifest(manifest_path) {
        Ok(manifest) => manifest,
        Err(err) => {
            let shown = sanitize(&err.to_string(), manifest_path, logical_root);
            return Err(format!("invalid manifest: {}", one_line(&shown)));
        }
    };
    let logical_manifest = format!("{logical_root}/phux-plugin.toml");
    let dead = plugin::dead_events(&manifest);
    if !dead.is_empty() {
        let detail = dead
            .iter()
            .map(|event| format!("{logical_manifest}: {event}"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("invalid manifest: {}", one_line(&detail)));
    }
    Ok(facts_of(&manifest))
}

pub(crate) fn facts_of(manifest: &PluginManifest) -> Facts {
    let mut kinds = Vec::new();
    if !manifest.actions.is_empty() {
        kinds.push("actions".to_owned());
    }
    if !manifest.agents.is_empty() {
        kinds.push("agents".to_owned());
    }
    if !manifest.events.is_empty() {
        kinds.push("events".to_owned());
    }
    if !manifest.links.is_empty() {
        kinds.push("links".to_owned());
    }
    if !manifest.panes.is_empty() {
        kinds.push("panes".to_owned());
    }
    if !manifest.profiles.is_empty() {
        kinds.push("profiles".to_owned());
    }
    if !manifest.sidebar.is_empty() {
        kinds.push("sidebar".to_owned());
    }
    if !manifest.themes.is_empty() {
        kinds.push("themes".to_owned());
    }
    if !manifest.widgets.is_empty() {
        kinds.push("widgets".to_owned());
    }
    Facts {
        id: manifest.id.clone(),
        name: cap_field(&manifest.name, NAME_MAX_CHARS),
        description: cap_field(
            &manifest.description.clone().unwrap_or_default(),
            DESCRIPTION_MAX_CHARS,
        ),
        min_phux_version: manifest.min_phux_version.clone(),
        kinds,
    }
}

/// Indexed `name` length, in Unicode scalars. Matches the schema `maxLength`.
pub(crate) const NAME_MAX_CHARS: usize = 200;
/// Indexed `description` length, in Unicode scalars.
pub(crate) const DESCRIPTION_MAX_CHARS: usize = 500;

pub(crate) fn cap_field(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars().take(max_chars).collect()
}

/// Theme palette paths (`themes/name/colors.toml`) declared by a manifest.
/// A document that does not parse contributes none; the loader reports that.
pub(crate) fn theme_color_rels(manifest: &str) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Probe {
        #[serde(default)]
        themes: Vec<ThemePath>,
    }
    #[derive(serde::Deserialize)]
    struct ThemePath {
        #[serde(default)]
        path: String,
    }
    let Ok(probe) = toml::from_str::<Probe>(manifest) else {
        return Vec::new();
    };
    probe
        .themes
        .into_iter()
        .filter_map(|theme| {
            safe_relative(&theme.path)
                .ok()
                .map(|path| format!("{path}/{COLORS_FILE}"))
        })
        .collect()
}

/// Relative directory of `[A-Za-z0-9._/-]` with no `.` or `..` components.
/// The error text never echoes `raw`, so a hostile name cannot enter the index.
pub(crate) fn safe_relative(raw: &str) -> Result<String, String> {
    let path = raw.trim();
    let allowed = |ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '/' | '-');
    if path.is_empty() || path.starts_with('/') || !path.chars().all(allowed) {
        return Err("invalid plugin directory".to_owned());
    }
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("invalid plugin directory".to_owned());
    }
    Ok(path.to_owned())
}

pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sanitize(err: &str, manifest_path: &Path, logical_root: &str) -> String {
    let mut needles = vec![manifest_path.to_path_buf()];
    if let Some(parent) = manifest_path.parent() {
        needles.push(parent.to_path_buf());
    }
    if let Ok(canonical) = manifest_path.canonicalize() {
        needles.push(canonical.clone());
        if let Some(parent) = canonical.parent() {
            needles.push(parent.to_path_buf());
        }
    }
    needles.sort_by_key(|path| std::cmp::Reverse(path.as_os_str().len()));
    needles.dedup();
    let mut out = err.to_owned();
    for needle in needles {
        let text = needle.display().to_string();
        if text.is_empty() {
            continue;
        }
        let replacement = if needle.ends_with("phux-plugin.toml") {
            format!("{logical_root}/phux-plugin.toml")
        } else {
            logical_root.to_owned()
        };
        out = out.replace(&text, &replacement);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(body: &str) -> (TempDir, PathBuf) {
        let dir = scratch().expect("temp");
        let path = dir.path().join("phux-plugin.toml");
        fs::write(&path, body).expect("write");
        (dir, path)
    }

    #[test]
    fn dead_event_is_the_plugin_validate_finding() {
        let (_dir, path) = write_manifest(
            r#"
id = "acme.broken"
name = "Broken"
version = "0.1.0"
min_phux_version = "0.0.2"

[[events]]
id = "nope"
title = "Nope"
on = "on-startup"
command = ["true"]
"#,
        );
        let err = judge(&path, "acme/broken").expect_err("dead event");
        assert_eq!(
            err,
            "invalid manifest: acme/broken/phux-plugin.toml: event `nope` listens for \
             `on-startup`, which is not a hook event; it will never fire"
        );
    }

    #[test]
    fn a_future_phux_requirement_is_rejected_by_the_loader() {
        let (_dir, path) = write_manifest(
            r#"
id = "acme.future"
name = "Future"
version = "0.1.0"
min_phux_version = "999.0.0"
"#,
        );
        let err = judge(&path, "acme/future").expect_err("too new");
        assert!(err.contains("requires phux >= 999.0.0"), "{err}");
        assert!(err.starts_with("invalid manifest:"), "{err}");
        assert!(!err.contains(path.to_str().unwrap_or("missing")), "{err}");
    }

    #[test]
    fn a_theme_without_colors_toml_is_rejected_by_the_loader() {
        let (_dir, path) = write_manifest(
            r#"
id = "acme.theme"
name = "Theme"
version = "0.1.0"
min_phux_version = "0.0.2"

[[themes]]
name = "demo"
path = "themes/demo"
"#,
        );
        let err = judge(&path, "acme/theme").expect_err("missing palette");
        assert!(err.contains("colors.toml"), "{err}");
        assert!(err.contains("acme/theme"), "{err}");
        assert!(
            !err.contains(
                path.parent()
                    .and_then(|parent| parent.to_str())
                    .unwrap_or("/tmp")
            ),
            "{err}"
        );
    }

    #[test]
    fn escaping_and_the_char_cap_are_pinned_together() {
        let name = "é".repeat(NAME_MAX_CHARS + 1);
        let description = "pipe|here <script> line\\nbreak tick`mark [click](javascript:alert(1)) https://evil.example";
        let body = format!(
            "id = \"acme.pin\"\nname = \"{name}\"\nversion = \"0.1.0\"\nmin_phux_version = \"0.0.2\"\ndescription = \"{description}\"\n"
        );
        let (_dir, path) = write_manifest(&body);
        let facts = judge(&path, "acme/pin").expect("valid");
        assert_eq!(facts.name, "é".repeat(NAME_MAX_CHARS));
        let markdown = crate::render_markdown(&crate::Index {
            schema_version: crate::SCHEMA_VERSION,
            generated_at: "2026-01-15T00:00:00Z".to_owned(),
            notice: crate::NOTICE.to_owned(),
            plugins: vec![crate::Plugin {
                repo: "acme/pin".to_owned(),
                subdir: String::new(),
                id: facts.id,
                name: facts.name,
                description: facts.description,
                kinds: facts.kinds,
                min_phux_version: facts.min_phux_version,
                stars: None,
                pushed_at: None,
                license: None,
                install: "phux plugin install https://github.com/acme/pin".to_owned(),
            }],
            skipped: Vec::new(),
        });
        assert!(markdown.contains("pipe\\|here"), "{markdown}");
        assert!(!markdown.contains("pipe|here"), "{markdown}");
        assert!(!markdown.contains("<script>"), "{markdown}");
        assert!(markdown.contains("\\<script\\>"), "{markdown}");
        assert!(markdown.contains("line break"), "{markdown}");
        assert!(!markdown.contains("line\nbreak"), "{markdown}");
        assert!(markdown.contains("tick\\`mark"), "{markdown}");
        assert!(!markdown.contains("](javascript:"), "{markdown}");
        assert!(!markdown.contains("https://evil.example"), "{markdown}");
        assert!(markdown.contains("https[:]//evil.example"), "{markdown}");
    }

    #[test]
    fn a_hostile_subdir_is_rejected_without_echoing_it() {
        let raw = "b\n# PWNED\n[click](//evil.example)";
        let err = safe_relative(raw).expect_err("hostile");
        assert_eq!(err, "invalid plugin directory");
        assert!(!err.contains('\n') && !err.contains("PWNED") && !err.contains("click"));
    }

    #[test]
    fn over_long_name_and_description_are_capped() {
        let name = "n".repeat(NAME_MAX_CHARS + 40);
        let description = "d".repeat(DESCRIPTION_MAX_CHARS + 40);
        let body = format!(
            "id = \"acme.long\"\nname = \"{name}\"\nversion = \"0.1.0\"\nmin_phux_version = \"0.0.2\"\ndescription = \"{description}\"\n"
        );
        let (_dir, path) = write_manifest(&body);
        let facts = judge(&path, "acme/long").expect("valid");
        assert_eq!(facts.name.chars().count(), NAME_MAX_CHARS);
        assert_eq!(facts.description.chars().count(), DESCRIPTION_MAX_CHARS);
        assert!(
            !facts
                .description
                .contains(&"d".repeat(DESCRIPTION_MAX_CHARS + 1))
        );
    }

    #[test]
    fn parent_directories_are_not_plugin_paths() {
        assert!(safe_relative("../escape").is_err());
        assert!(safe_relative("/etc").is_err());
        assert!(safe_relative("plugins/theme").is_ok());
    }
}
