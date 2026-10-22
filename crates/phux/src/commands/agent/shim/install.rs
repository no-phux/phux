//! Installing and removing the shim on disk: where it lives, the real
//! `claude` it wraps, its manifest, and the atomic write.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use crate::commands::sh_quote;
use crate::companion::is_executable;

use super::rc::{install_rc_block, remove_rc_block, shell_activation, shell_rc};
use super::wrapper::render_wrapper;
use super::{HOOKS, InstallReport, MANIFEST, SCHEMA_MARKER, SHIM_SCHEMA};

/// Where the wrapper installed as `claude` lives for this user.
pub(super) fn shim_dir_for(home: &Path) -> PathBuf {
    data_home(home).join("phux").join("shims")
}

/// The installed wrapper's path (for `phux doctor`), or `None` without `HOME`.
pub(crate) fn installed_shim_path() -> Option<PathBuf> {
    Some(shim_dir_for(&home_dir().ok()?).join("claude"))
}

pub(super) fn install_claude(
    shell: &str,
    explicit_real: Option<&Path>,
) -> Result<InstallReport, String> {
    let home = home_dir()?;
    let shim_dir = shim_dir_for(&home);
    let rc = shell_rc(shell, &home)?;
    let phux = std::env::current_exe()
        .map_err(|err| format!("could not resolve the running phux binary: {err}"))?;
    install_claude_into(&shim_dir, &rc, shell, explicit_real, &phux)
}

/// `install_claude` with every ambient path handed in, so tests can run it
/// in a tempdir without mutating the process environment.
pub(super) fn install_claude_into(
    shim_dir: &Path,
    rc: &Path,
    shell: &str,
    explicit_real: Option<&Path>,
    phux: &Path,
) -> Result<InstallReport, String> {
    let shim = shim_dir.join("claude");
    let settings = shim_dir.join("claude-hooks.json");
    let manifest = shim_dir.join(MANIFEST);
    let replaced = installed_shim_schema(&shim);
    let real = resolve_real_claude(explicit_real, shim_dir, &manifest)?;
    let (shim_dir, rc) = (shim_dir.to_path_buf(), rc.to_path_buf());

    std::fs::create_dir_all(&shim_dir)
        .map_err(|err| format!("could not create {}: {err}", shim_dir.display()))?;

    let hook_command =
        |arm: &str| format!("{} --phux-hook {arm}", sh_quote(&shim.to_string_lossy()));
    let hooks: serde_json::Map<String, serde_json::Value> = HOOKS
        .iter()
        .map(|(event, matcher, arm)| {
            let entry = serde_json::json!([{
                "matcher": matcher,
                "hooks": [{ "type": "command", "command": hook_command(arm) }]
            }]);
            ((*event).to_owned(), entry)
        })
        .collect();
    let hook_settings = serde_json::json!({ "hooks": hooks });
    let settings_bytes = serde_json::to_vec_pretty(&hook_settings)
        .map_err(|err| format!("could not render Claude hook settings: {err}"))?;
    atomic_write(&settings, &settings_bytes, 0o600)?;

    let wrapper = render_wrapper(&real, phux, &shim, &settings)?;
    atomic_write(&shim, wrapper.as_bytes(), 0o755)?;

    // `schema_version` versions the manifest's shape, `shim_schema` the
    // wrapper's behavior. Readers pull single keys, so v1 still uninstalls.
    let manifest_value = serde_json::json!({
        "schema_version": 2,
        "shim_schema": SHIM_SCHEMA,
        "real_claude": real,
        "shell": shell,
        "rc": rc,
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest_value)
        .map_err(|err| format!("could not render shim manifest: {err}"))?;
    atomic_write(&manifest, &manifest_bytes, 0o600)?;

    let activation = shell_activation(shell, &shim_dir)?;
    install_rc_block(&rc, &activation)?;

    Ok(InstallReport { shim, rc, replaced })
}

pub(super) fn uninstall_claude() -> Result<Option<PathBuf>, String> {
    let shim_dir = shim_dir_for(&home_dir()?);
    uninstall_claude_from(&shim_dir)
}

/// `uninstall_claude` with the shim directory handed in. Removes exactly the
/// three files [`install_claude_into`] writes plus the marked rc block.
pub(super) fn uninstall_claude_from(shim_dir: &Path) -> Result<Option<PathBuf>, String> {
    let manifest = shim_dir.join(MANIFEST);
    let Some(value) = read_manifest(&manifest)? else {
        return Ok(None);
    };
    let rc = value
        .get("rc")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{} has no rc path", manifest.display()))?;

    remove_rc_block(&rc)?;
    for path in [
        shim_dir.join("claude"),
        shim_dir.join("claude-hooks.json"),
        manifest,
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("could not remove {}: {err}", path.display())),
        }
    }
    Ok(Some(rc))
}

pub(super) fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "cannot install a shell shim because HOME is unset".to_owned())
}

pub(super) fn data_home(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".local").join("share"), PathBuf::from)
}

pub(super) fn resolve_real_claude(
    explicit: Option<&Path>,
    shim_dir: &Path,
    manifest: &Path,
) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return validate_executable(path, shim_dir);
    }
    if let Some(value) = read_manifest(manifest)?
        && let Some(path) = value.get("real_claude").and_then(serde_json::Value::as_str)
    {
        return validate_executable(Path::new(path), shim_dir);
    }

    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from(""));
    for dir in std::env::split_paths(&path) {
        if dir == shim_dir {
            continue;
        }
        let candidate = dir.join("claude");
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err("could not find the real `claude` on PATH; pass --real /absolute/path/to/claude".to_owned())
}

pub(super) fn validate_executable(path: &Path, shim_dir: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "real Claude path must be absolute: {}",
            path.display()
        ));
    }
    if path.parent() == Some(shim_dir) {
        return Err("real Claude path resolves to the phux shim itself".to_owned());
    }
    if !is_executable(path) {
        return Err(format!(
            "real Claude binary is not executable: {}",
            path.display()
        ));
    }
    Ok(path.to_path_buf())
}

pub(super) fn read_manifest(path: &Path) -> Result<Option<serde_json::Value>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| format!("could not parse {}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("could not read {}: {err}", path.display())),
    }
}

/// The schema of the shim already on disk, or `None` when none is readable.
/// An unstamped wrapper predates stamping and is schema 1.
pub(crate) fn installed_shim_schema(shim: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(shim).ok()?;
    Some(
        text.lines()
            .find_map(|line| line.strip_prefix(SCHEMA_MARKER))
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(1),
    )
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|err| format!("could not create {}: {err}", tmp.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|err| format!("could not chmod {}: {err}", tmp.display()))?;
        file.write_all(bytes)
            .map_err(|err| format!("could not write {}: {err}", tmp.display()))?;
        file.sync_all()
            .map_err(|err| format!("could not sync {}: {err}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|err| format!("could not replace {}: {err}", path.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
