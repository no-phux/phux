//! `phux plugin new NAME [--dir PATH]`.
//!
//! Writes a minimal plugin: `phux-plugin.toml` (name, version,
//! `min_phux_version` of this phux, one action, one event hook), a README,
//! and the script the action runs. A non-empty directory is left untouched.
//! Generated prose spells phux in lowercase; `PHUX_*` names are the
//! environment variables the runtime actually sets.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_config::plugin::CURRENT_PHUX_VERSION;

use super::fail;

const PLUGIN_VERSION: &str = "0.1.0";
const PLUGIN_ID_MAX_CHARS: usize = 120;

/// A plugin directory `phux plugin new` wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScaffoldedPlugin {
    /// Manifest `id` and `name`.
    pub(super) id: String,
    /// Directory that holds `phux-plugin.toml`.
    pub(super) dir: PathBuf,
    /// Manifest `version`.
    pub(super) version: String,
    /// Manifest `min_phux_version`, this phux's version.
    pub(super) min_phux_version: String,
}

/// `phux plugin new NAME [--dir PATH] [--json]`.
pub(super) fn run_new(name: &str, dir: Option<&Path>, json: bool) -> ExitCode {
    let destination = match destination_dir(name, dir) {
        Ok(dir) => dir,
        Err(err) => return fail(json, &err),
    };
    match scaffold_plugin(name, &destination) {
        Ok(created) => report_created(&created, json),
        Err(err) => fail(json, &err),
    }
}

/// Write a minimal plugin at `dir`. Refuses a non-empty directory.
pub(super) fn scaffold_plugin(name: &str, dir: &Path) -> Result<ScaffoldedPlugin, String> {
    validate_plugin_id(name)?;
    let created_dir = prepare_dir(dir)?;
    let files = render_files(name);
    if let Err(err) = write_files(dir, &files) {
        cleanup(dir, created_dir);
        return Err(err);
    }
    Ok(ScaffoldedPlugin {
        id: name.to_owned(),
        dir: dir.to_path_buf(),
        version: PLUGIN_VERSION.to_owned(),
        min_phux_version: CURRENT_PHUX_VERSION.to_owned(),
    })
}

fn destination_dir(name: &str, dir: Option<&Path>) -> Result<PathBuf, String> {
    validate_plugin_id(name)?;
    match dir {
        Some(dir) if dir.as_os_str().is_empty() => Err("--dir must be a path".to_owned()),
        Some(dir) => Ok(dir.to_path_buf()),
        None => {
            let cwd = std::env::current_dir()
                .map_err(|err| format!("could not resolve the working directory: {err}"))?;
            Ok(cwd.join(name))
        }
    }
}

fn validate_plugin_id(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= PLUGIN_ID_MAX_CHARS
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "plugin name {name:?} is not a plugin id: use ASCII letters, digits, \
             '.', '_', '-', or ':' (at most {PLUGIN_ID_MAX_CHARS} characters)"
        ))
    }
}

/// `true` when this call created `dir` and a later failure should remove it.
fn prepare_dir(dir: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "{} is a symlink; phux plugin new refuses to write through it",
            dir.display()
        )),
        Ok(meta) if meta.is_dir() => {
            let mut entries = std::fs::read_dir(dir)
                .map_err(|err| format!("could not read {}: {err}", dir.display()))?;
            if entries.next().is_some() {
                Err(format!(
                    "{} is not empty; phux plugin new refuses to overwrite a non-empty directory",
                    dir.display()
                ))
            } else {
                Ok(false)
            }
        }
        Ok(_) => Err(format!("{} exists and is not a directory", dir.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)
                .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
            Ok(true)
        }
        Err(err) => Err(format!("could not inspect {}: {err}", dir.display())),
    }
}

struct RenderedFiles {
    manifest: String,
    readme: String,
    script: String,
}

fn render_files(name: &str) -> RenderedFiles {
    let manifest = format!(
        r#"id = "{name}"
name = "{name}"
version = "{PLUGIN_VERSION}"
min_phux_version = "{CURRENT_PHUX_VERSION}"
description = "A minimal phux plugin."

[[actions]]
id = "hello"
title = "Say hello"
description = "Prints a one-line greeting."
contexts = ["workspace"]
command = ["sh", "scripts/hello.sh"]

[[events]]
id = "log-pane-exit"
title = "Log pane exits"
description = "Appends each pane exit to pane-exits.log."
on = "pane-exit"
command = ["sh", "-c", "printf '%s %s\\n' \"$PHUX_EVENT\" \"${{PHUX_EXIT_CODE:-}}\" >> pane-exits.log"]
"#
    );
    let readme = format!(
        r"# {name}

A minimal phux plugin. The `hello` action runs `scripts/hello.sh`.
The `log-pane-exit` hook runs when a pane exits.

```sh
phux plugin validate .
phux plugin link .
phux config reload
```

Hooks are read when the server starts. After linking, run `phux upgrade`
to restart it in place.
"
    );
    let script = "\
#!/bin/sh
# Example action. phux runs this from the plugin directory.
set -eu
printf 'hello from %s\\n' \"${PHUX_PLUGIN_ID:-this plugin}\"
"
    .to_owned();
    RenderedFiles {
        manifest,
        readme,
        script,
    }
}

fn write_files(dir: &Path, files: &RenderedFiles) -> Result<(), String> {
    let scripts = dir.join("scripts");
    std::fs::create_dir_all(&scripts)
        .map_err(|err| format!("could not create {}: {err}", scripts.display()))?;
    write_text(&dir.join("phux-plugin.toml"), &files.manifest)?;
    write_text(&dir.join("README.md"), &files.readme)?;
    let script = scripts.join("hello.sh");
    write_text(&script, &files.script)?;
    set_executable(&script)?;
    Ok(())
}

fn write_text(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents)
        .map_err(|err| format!("could not write {}: {err}", path.display()))
}

fn set_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)
            .map_err(|err| format!("could not inspect {}: {err}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)
            .map_err(|err| format!("could not mark {} executable: {err}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn cleanup(dir: &Path, created_dir: bool) {
    if created_dir {
        let _ = std::fs::remove_dir_all(dir);
        return;
    }
    let _ = std::fs::remove_file(dir.join("phux-plugin.toml"));
    let _ = std::fs::remove_file(dir.join("README.md"));
    let _ = std::fs::remove_file(dir.join("scripts").join("hello.sh"));
    let _ = std::fs::remove_dir(dir.join("scripts"));
}

fn report_created(created: &ScaffoldedPlugin, json: bool) -> ExitCode {
    if json {
        let doc = serde_json::json!({
            "schema_version": 1,
            "created": {
                "id": created.id,
                "name": created.id,
                "version": created.version,
                "min_phux_version": created.min_phux_version,
                "dir": created.dir,
            },
        });
        return crate::output::json(&doc);
    }
    outln!("created {} in {}", created.id, created.dir.display());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::scaffold_plugin;
    use phux_config::plugin::{self, CURRENT_PHUX_VERSION};

    #[test]
    fn scaffold_validates_and_spells_phux_in_lowercase() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("hello");
        let created = scaffold_plugin("hello", &dir).unwrap();
        assert_eq!(created.id, "hello");
        assert_eq!(created.min_phux_version, CURRENT_PHUX_VERSION);
        assert_eq!(created.version, "0.1.0");

        let manifest = plugin::load_plugin_manifest(&dir).unwrap();
        assert_eq!(manifest.id, "hello");
        assert_eq!(manifest.name, "hello");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.min_phux_version, CURRENT_PHUX_VERSION);
        assert_eq!(manifest.actions.len(), 1);
        assert_eq!(manifest.actions[0].id, "hello");
        assert_eq!(
            manifest.actions[0].command,
            vec!["sh".to_owned(), "scripts/hello.sh".to_owned()]
        );
        assert_eq!(manifest.events.len(), 1);
        assert_eq!(manifest.events[0].on, "pane-exit");
        assert!(plugin::dead_events(&manifest).is_empty());
        assert!(dir.join("scripts").join("hello.sh").is_file());

        for relative in ["phux-plugin.toml", "README.md", "scripts/hello.sh"] {
            let text = std::fs::read_to_string(dir.join(relative)).unwrap();
            let scrubbed = text.replace("PHUX_", "");
            assert!(!scrubbed.contains("Phux"), "{relative}: {text}");
            assert!(!scrubbed.contains("PHUX"), "{relative}: {text}");
        }

        let err = scaffold_plugin("hello", &dir).unwrap_err();
        assert!(err.contains("not empty"), "{err}");
        assert!(dir.join("phux-plugin.toml").is_file());

        let empty = root.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        scaffold_plugin("com.example.tools", &empty).unwrap();
        plugin::load_plugin_manifest(&empty).unwrap();

        let bad = scaffold_plugin("Not A Plugin", &root.path().join("nope")).unwrap_err();
        assert!(bad.contains("not a plugin id"), "{bad}");
        assert!(!root.path().join("nope").exists());
    }
}
