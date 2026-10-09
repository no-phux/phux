//! `phux theme <action>`: the theme catalog (ADR-0157).
//!
//! A theme is a directory holding an Omarchy-schema `colors.toml`. Names
//! resolve through [`phux_config::theme::catalog`]: the managed directory
//! (`$XDG_DATA_HOME/phux/themes/<name>`) first, then the `[[themes]]` of
//! enabled plugins. `set` edits the one `[theme] name` / `file` key in
//! `config.toml` through the settings writer and rings the reload doorbell
//! so attached clients repaint.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_config::loader as config_loader;
use phux_config::plugin::PluginManifest;
use phux_config::settings::Edit;
use phux_config::theme::{self, FILE_KEY, NAME_KEY, ThemeEntry, ThemeSelection, ThemeSource};

use crate::commands::ThemeAction;

pub(crate) fn run_theme(action: &ThemeAction, socket: Option<PathBuf>) -> ExitCode {
    let json = match action {
        ThemeAction::List { json }
        | ThemeAction::Show { json, .. }
        | ThemeAction::Install { json, .. } => *json,
        ThemeAction::Set { .. } | ThemeAction::Remove { .. } => false,
    };
    let result = match action {
        ThemeAction::List { json } => list(*json),
        ThemeAction::Show { name, json } => show(name.as_deref(), *json),
        ThemeAction::Set { name, file } => set(name.as_deref(), file.as_deref(), socket),
        ThemeAction::Install { source, json } => install(source, *json),
        ThemeAction::Remove { name } => remove(name),
    };
    result.unwrap_or_else(|err| fail(json, &err))
}

/// The config and the manifests of its enabled plugins.
fn load_config() -> Result<(phux_config::Config, Vec<PluginManifest>), String> {
    let cfg = config_loader::load().map_err(|err| err.to_string())?;
    let manifests =
        phux_config::plugin::load_enabled_manifests(&config_loader::config_path(), &cfg.plugins);
    Ok((cfg, manifests))
}

fn list(json: bool) -> Result<ExitCode, String> {
    let (cfg, manifests) = load_config()?;
    let entries = theme::catalog(&manifests);
    let active = match cfg.theme.selection() {
        ThemeSelection::Name(name) => Some(name.to_owned()),
        ThemeSelection::File(_) | ThemeSelection::None => None,
    };
    if json {
        let doc = serde_json::json!({
            "schema_version": 1,
            "active": active,
            "file": match cfg.theme.selection() {
                ThemeSelection::File(file) => Some(file),
                _ => None,
            },
            "themes": entries,
        });
        return Ok(crate::output::json(&doc));
    }
    if let ThemeSelection::File(file) = cfg.theme.selection() {
        outln!("* (file) {file}");
    }
    if entries.is_empty() {
        outln!("No themes installed. Install one with `phux theme install URL`.");
        return Ok(ExitCode::SUCCESS);
    }
    for entry in entries {
        let marker = if active.as_deref() == Some(entry.name.as_str()) {
            '*'
        } else {
            ' '
        };
        match &entry.source {
            ThemeSource::Installed => outln!("{marker} {}", entry.name),
            ThemeSource::Plugin { plugin } => outln!("{marker} {} (plugin {plugin})", entry.name),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn show(name: Option<&str>, json: bool) -> Result<ExitCode, String> {
    let (cfg, manifests) = load_config()?;
    let (label, dir, source, colors) = match (name, cfg.theme.selection()) {
        (Some(name), _) | (None, ThemeSelection::Name(name)) => {
            let entry = theme::find(name, &manifests).map_err(|err| err.to_string())?;
            let colors = theme::load_dir(&entry.dir).map_err(|err| err.to_string())?;
            (entry.name, entry.dir, Some(entry.source), colors)
        }
        (None, ThemeSelection::File(file)) => {
            let path = theme::expand_home(file);
            let colors = theme::load_file(&path).map_err(|err| err.to_string())?;
            (file.to_owned(), path, None, colors)
        }
        (None, ThemeSelection::None) => {
            return Err("no theme selected; name one, or `phux theme set NAME`".to_owned());
        }
    };
    let contrast = colors.foreground().contrast_ratio(colors.background());
    if json {
        let doc = serde_json::json!({
            "schema_version": 1,
            "theme": {
                "name": label,
                "path": dir,
                "source": source,
                "mode": colors.mode,
                "colors": colors.colors.iter().map(|(k, v)| (k, v.hex())).collect::<std::collections::BTreeMap<_, _>>(),
                "ansi16": colors.ansi16().iter().map(|c| c.hex()).collect::<Vec<_>>(),
                "contrast": { "foreground_on_background": contrast },
            },
        });
        return Ok(crate::output::json(&doc));
    }
    outln!("{label} ({:?}, {})", colors.mode, dir.display());
    outln!(
        "foreground on background: {contrast:.2}:1{}",
        if contrast < 4.5 {
            " (below WCAG AA 4.5:1)"
        } else {
            ""
        }
    );
    for (key, value) in &colors.colors {
        outln!("{key} = {}", value.hex());
    }
    let palette: Vec<String> = colors.ansi16().iter().map(|c| c.hex()).collect();
    outln!("palette = {}", palette.join(" "));
    Ok(ExitCode::SUCCESS)
}

fn set(
    name: Option<&str>,
    file: Option<&Path>,
    socket: Option<PathBuf>,
) -> Result<ExitCode, String> {
    if name.is_some() == file.is_some() {
        return Err("pass exactly one of NAME or --file PATH".to_owned());
    }
    let (_, manifests) = load_config()?;
    let (set_key, unset_key, value, label) = match (name, file) {
        (Some(name), _) => {
            let entry = theme::find(name, &manifests).map_err(|err| err.to_string())?;
            theme::load_dir(&entry.dir).map_err(|err| err.to_string())?;
            (NAME_KEY, FILE_KEY, entry.name.clone(), entry.name)
        }
        (None, Some(file)) => {
            let text = file
                .to_str()
                .ok_or("theme file path must be UTF-8")?
                .to_owned();
            theme::load_file(&theme::expand_home(&text)).map_err(|err| err.to_string())?;
            (FILE_KEY, NAME_KEY, text.clone(), text)
        }
        (None, None) => unreachable!("checked above"),
    };
    let config_path = config_loader::config_path();
    let edit = |key: &str, edit: Edit| {
        phux_config::settings::write_edit(&config_path, &format!("theme.{key}"), edit)
            .map_err(|err| err.to_string())
    };
    edit(unset_key, Edit::Unset)?;
    edit(set_key, Edit::Set(toml::Value::String(value)))?;
    outln!("theme = {label}");

    // Repaint attached clients; no server is not a failure of `set`.
    let socket_path = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    let rt = crate::commands::cli_runtime().map_err(|_| "no tokio runtime".to_owned())?;
    match rt.block_on(super::config::ring_config_reload(&socket_path)) {
        Ok(()) => outln!("reload signalled to attached clients"),
        Err(super::config::ReloadRingError::NoServer(_)) => {
            outln!(
                "no running server at {}; clients pick it up on attach",
                socket_path.display()
            );
        }
        Err(super::config::ReloadRingError::Unconfirmed(refusal)) => {
            eprintln!("phux: config reload could not be confirmed: {refusal}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `phux theme install SOURCE`: a git URL (cloned with the system `git`) or a
/// local directory (copied), holding `colors.toml` at its root. The result
/// lands at `<themes dir>/<name>`, replacing a previous install of that name.
fn install(source: &str, json: bool) -> Result<ExitCode, String> {
    let themes_dir = theme::themes_dir()
        .ok_or("cannot resolve the phux data dir: neither XDG_DATA_HOME nor HOME is set")?;
    phux_config::production::refuse_dev_on_production_state(&themes_dir)?;
    std::fs::create_dir_all(&themes_dir)
        .map_err(|err| format!("could not create {}: {err}", themes_dir.display()))?;

    let local = Path::new(source);
    let is_dir = !source.contains("://") && local.is_dir();
    let name = if is_dir {
        local
            .canonicalize()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .and_then(|base| theme::name_from_url(&base))
    } else {
        theme::name_from_url(source)
    }
    .ok_or_else(|| format!("{source:?} does not give a usable theme name"))?;

    let staging = themes_dir.join(format!(".staging-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let staged = if is_dir {
        copy_theme_dir(local, &staging)
    } else {
        clone_theme(source, &staging)
    };
    let colors = staged.and_then(|()| {
        if !staging.join(theme::COLORS_FILE).is_file() {
            return Err(format!(
                "{source} has no {} at its root",
                theme::COLORS_FILE
            ));
        }
        theme::load_dir(&staging).map_err(|err| err.to_string())
    });
    let colors = match colors {
        Ok(colors) => colors,
        Err(err) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
    };

    let final_dir = themes_dir.join(&name);
    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir)
            .map_err(|err| format!("could not replace {}: {err}", final_dir.display()))?;
    }
    std::fs::rename(&staging, &final_dir)
        .map_err(|err| format!("could not move theme into {}: {err}", final_dir.display()))?;

    let entry = ThemeEntry {
        name: name.clone(),
        dir: final_dir,
        source: ThemeSource::Installed,
    };
    if json {
        let doc = serde_json::json!({
            "schema_version": 1,
            "installed": entry,
            "mode": colors.mode,
        });
        return Ok(crate::output::json(&doc));
    }
    outln!(
        "installed {name} ({:?}) -> `phux theme set {name}`",
        colors.mode
    );
    Ok(ExitCode::SUCCESS)
}

fn clone_theme(url: &str, staging: &Path) -> Result<(), String> {
    if url.starts_with('-') {
        return Err(format!("{url:?} is not a git URL"));
    }
    let output = std::process::Command::new("git")
        .args(["clone", "--depth", "1", "--"])
        .arg(url)
        .arg(staging)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|err| format!("could not spawn git clone: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "git clone failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // A theme is data; the install is a snapshot, not a working clone.
    let _ = std::fs::remove_dir_all(staging.join(".git"));
    Ok(())
}

/// Copy a theme directory's regular files (no symlinks, no `.git`); a theme
/// is colours and previews, nothing that needs more.
fn copy_theme_dir(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst)
        .map_err(|err| format!("could not create {}: {err}", dst.display()))?;
    for entry in
        std::fs::read_dir(src).map_err(|err| format!("could not read {}: {err}", src.display()))?
    {
        let entry = entry.map_err(|err| err.to_string())?;
        let kind = entry.file_type().map_err(|err| err.to_string())?;
        let name = entry.file_name();
        if name == ".git" || kind.is_symlink() {
            continue;
        }
        let target = dst.join(&name);
        if kind.is_dir() {
            copy_theme_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)
                .map_err(|err| format!("could not copy {}: {err}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn remove(name: &str) -> Result<ExitCode, String> {
    let (_, manifests) = load_config()?;
    let entry = theme::find(name, &manifests).map_err(|err| err.to_string())?;
    match entry.source {
        ThemeSource::Plugin { plugin } => Err(format!(
            "{name} is provided by plugin {plugin}; `phux plugin disable {plugin}` removes it"
        )),
        ThemeSource::Installed => {
            std::fs::remove_dir_all(&entry.dir)
                .map_err(|err| format!("could not remove {}: {err}", entry.dir.display()))?;
            outln!("removed {name}");
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn fail(json: bool, message: &str) -> ExitCode {
    if json {
        return crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::REGISTRY,
                message,
                "run `phux theme list` to see installed themes",
            ),
            1,
        );
    }
    eprintln!("phux: {message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colors_only_directory_copies_without_git_or_symlinks() {
        let base = std::env::temp_dir().join(format!("phux-theme-copy-{}", std::process::id()));
        let src = base.join("src");
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::create_dir_all(src.join("backgrounds")).unwrap();
        std::fs::write(src.join("colors.toml"), "mode = \"dark\"\n").unwrap();
        std::fs::write(src.join("backgrounds/1.png"), b"png").unwrap();
        std::fs::write(src.join(".git/HEAD"), "ref").unwrap();
        let dst = base.join("dst");
        copy_theme_dir(&src, &dst).unwrap();
        assert!(dst.join("colors.toml").is_file());
        assert!(dst.join("backgrounds/1.png").is_file());
        assert!(!dst.join(".git").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_flag_shaped_url_is_refused_before_git_sees_it() {
        let err = clone_theme("--upload-pack=x", Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("not a git URL"));
    }

    #[test]
    fn set_needs_exactly_one_target() {
        assert!(set(None, None, None).is_err());
        assert!(set(Some("a"), Some(Path::new("b")), None).is_err());
    }
}
