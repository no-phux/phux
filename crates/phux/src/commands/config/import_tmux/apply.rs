//! Publish a native layer using the same locked, comment-preserving config
//! transaction as the machine/plugin registries. The user's file wins last.

use std::io::Write;
use std::path::{Path, PathBuf};

use toml_edit::{Array, Item, Value};

pub(super) const LAYER_NAME: &str = "tmux-import.toml";

/// Write exclusively: neither an existing file nor a symlink is overwritten.
pub(super) fn save_new(path: &Path, contents: &str) -> Result<(), String> {
    phux_config::production::refuse_dev_on_production_state(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            format!(
                "cannot create {} (existing imports are never overwritten): {e}",
                path.display()
            )
        })?;
    let result = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(err) = result {
        let _ = std::fs::remove_file(path);
        return Err(format!("could not write {}: {err}", path.display()));
    }
    Ok(())
}

pub(super) fn install(config: &Path, contents: &str) -> Result<PathBuf, String> {
    let mut edit = phux_config::toml_registry::edit_document(config)?;
    let layer = config.with_file_name(LAYER_NAME);
    if layer.try_exists().map_err(|e| e.to_string())? {
        return Err(format!(
            "{} already exists; review/remove the previous import before applying again",
            layer.display()
        ));
    }
    prepend_layer(&mut edit)?;
    save_new(&layer, contents)?;
    if let Err(err) = validate(&edit.to_string(), config) {
        std::fs::remove_file(&layer)
            .map_err(|cleanup| format!("{err}; could not remove {}: {cleanup}", layer.display()))?;
        return Err(err);
    }
    // If publication detects an external edit, that editor may now reference
    // the layer. Keep it rather than making their config point at a deleted
    // file. The shared writer still refuses to overwrite their root edit.
    edit.commit().map_err(|err| {
        format!(
            "{err}; import layer retained at {}; review it before retrying",
            layer.display()
        )
    })?;
    Ok(layer)
}

fn prepend_layer(doc: &mut toml_edit::DocumentMut) -> Result<(), String> {
    if let Some(item) = doc.get_mut("extends") {
        let array = item.as_array_mut().ok_or("extends must be an array")?;
        if array.iter().any(|v| v.as_str() == Some(LAYER_NAME)) {
            return Err(
                "config already extends tmux-import.toml; restore/review that import first".into(),
            );
        }
        array.insert(0, LAYER_NAME);
    } else {
        let mut array = Array::new();
        array.push(LAYER_NAME);
        // A root key must precede the first [table]. toml_edit renders root
        // values before tables while preserving their comments.
        doc.insert("extends", Item::Value(Value::Array(array)));
    }
    Ok(())
}

fn validate(text: &str, path: &Path) -> Result<(), String> {
    let report = phux_config::check::check(text, path).map_err(|e| e.to_string())?;
    if !report.is_ok() {
        return Err(format!(
            "merged config is invalid; original config preserved: {:?}",
            report.findings
        ));
    }
    // Also require the real typed loader to accept the settled layer stack.
    phux_config::parse_with_defaults(text, path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_user_values_comments_layers_and_undo() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(
            dir.path().join("work.toml"),
            "[keybindings]\nprefix = 'C-z'\n",
        )
        .unwrap();
        std::fs::write(
            &config,
            "# my config\nextends = ['work.toml'] # work\n[sidebar]\nwidth = 31\n",
        )
        .unwrap();
        let original = std::fs::read_to_string(&config).unwrap();
        let layer = install(&config, "[keybindings]\nprefix = 'C-b'\n").unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("# work"));
        let loaded = phux_config::parse_with_defaults(&text, &config).unwrap();
        assert_eq!(loaded.keybindings.prefix, "C-z");
        assert_eq!(loaded.sidebar.width, 31);
        assert!(install(&config, "").is_err());
        std::fs::write(&config, &original).unwrap();
        std::fs::remove_file(layer).unwrap();
        assert_eq!(std::fs::read_to_string(config).unwrap(), original);
    }

    #[test]
    fn import_preserves_managed_distro_selection_switching_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let first = dir.path().join("first.toml");
        let second = dir.path().join("second.toml");
        std::fs::write(&first, "[keybindings]\nprefix = 'C-z'\n").unwrap();
        std::fs::write(&second, "[keybindings]\nprefix = 'C-y'\n").unwrap();
        phux_config::distro::switch_distro(&config, Some(&first)).unwrap();
        let layer = install(&config, "[keybindings]\nprefix = 'C-b'\n").unwrap();
        assert_eq!(
            phux_config::distro::current_distro(&config).unwrap(),
            Some(first.canonicalize().unwrap())
        );
        phux_config::distro::switch_distro(&config, Some(&second)).unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        assert_eq!(
            phux_config::parse_with_defaults(&text, &config)
                .unwrap()
                .keybindings
                .prefix,
            "C-y"
        );
        phux_config::distro::switch_distro(&config, None).unwrap();
        assert!(layer.exists());
        assert!(
            phux_config::distro::current_distro(&config)
                .unwrap()
                .is_none()
        );
        let text = std::fs::read_to_string(&config).unwrap();
        assert_eq!(
            phux_config::parse_with_defaults(&text, &config)
                .unwrap()
                .keybindings
                .prefix,
            "C-b"
        );
    }

    #[test]
    fn invalid_merge_and_existing_layer_leave_original_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[sidebar]\nwidth = 'invalid'\n").unwrap();
        let original = std::fs::read_to_string(&config).unwrap();
        assert!(install(&config, "[keybindings]\nprefix = 'C-b'\n").is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
        assert!(!dir.path().join(LAYER_NAME).exists());
        std::fs::write(dir.path().join(LAYER_NAME), "keep").unwrap();
        assert!(install(&config, "replace").is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(LAYER_NAME)).unwrap(),
            "keep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn follows_config_symlink_and_refuses_output_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dotfiles.toml");
        let config = dir.path().join("config.toml");
        std::fs::write(&target, "# dotfiles\n").unwrap();
        std::os::unix::fs::symlink(&target, &config).unwrap();
        install(&config, "[keybindings]\nprefix = 'C-b'\n").unwrap();
        assert!(std::fs::symlink_metadata(&config).unwrap().is_symlink());
        assert!(
            std::fs::read_to_string(&target)
                .unwrap()
                .contains("extends")
        );
        let output = dir.path().join("output.toml");
        std::os::unix::fs::symlink(&target, &output).unwrap();
        assert!(save_new(&output, "replace").is_err());
    }
}
