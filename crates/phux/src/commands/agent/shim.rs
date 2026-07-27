//! Install the `claude` adoption shim: plain `claude` becomes a phux session.
//!
//! The shim is deliberately installed into a phux-owned directory and activated
//! by one bounded shell-rc block. It never overwrites the real Claude binary.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const BLOCK_BEGIN: &str = "# >>> phux agent shims >>>";
const BLOCK_END: &str = "# <<< phux agent shims <<<";
const MANIFEST: &str = "claude-install.json";

pub(super) fn run_install_claude(shell: Option<&str>, real: Option<&Path>) -> ExitCode {
    let shell = shell.map_or_else(detected_shell, str::to_owned);
    match install_claude(&shell, real) {
        Ok(report) => {
            outln!("installed claude-in-phux shim at {}", report.shim.display());
            outln!("activated it in {}", report.rc.display());
            outln!("open a new shell, then plain `claude` launches inside phux");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err),
    }
}

pub(super) fn run_uninstall_claude() -> ExitCode {
    match uninstall_claude() {
        Ok(Some(rc)) => {
            outln!(
                "removed claude-in-phux shim and activation from {}",
                rc.display()
            );
            ExitCode::SUCCESS
        }
        Ok(None) => {
            outln!("claude-in-phux shim is not installed");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err),
    }
}

fn detected_shell() -> String {
    std::env::var_os("SHELL")
        .and_then(|path| PathBuf::from(path).file_name().map(ToOwned::to_owned))
        .and_then(|name| name.to_str().map(str::to_owned))
        .filter(|name| matches!(name.as_str(), "zsh" | "bash" | "fish"))
        .unwrap_or_else(|| "zsh".to_owned())
}

fn fail(message: &str) -> ExitCode {
    eprintln!("phux agent: {message}");
    ExitCode::FAILURE
}

struct InstallReport {
    shim: PathBuf,
    rc: PathBuf,
}

fn install_claude(shell: &str, explicit_real: Option<&Path>) -> Result<InstallReport, String> {
    let home = home_dir()?;
    let shim_dir = data_home(&home).join("phux").join("shims");
    let shim = shim_dir.join("claude");
    let settings = shim_dir.join("claude-hooks.json");
    let manifest = shim_dir.join(MANIFEST);
    let phux = std::env::current_exe()
        .map_err(|err| format!("could not resolve the running phux binary: {err}"))?;
    let real = resolve_real_claude(explicit_real, &shim_dir, &manifest)?;
    let rc = shell_rc(shell, &home)?;

    std::fs::create_dir_all(&shim_dir)
        .map_err(|err| format!("could not create {}: {err}", shim_dir.display()))?;

    let hook_command = |state: &str| format!("{} --phux-hook {state}", sh_quote_path(&shim));
    let command_hook = |state: &str, matcher: &str| {
        serde_json::json!({
            "matcher": matcher,
            "hooks": [{ "type": "command", "command": hook_command(state) }]
        })
    };
    let hook_settings = serde_json::json!({
        "hooks": {
            "SessionStart": [command_hook("idle", "")],
            "UserPromptSubmit": [command_hook("working", "")],
            "PermissionRequest": [command_hook("blocked", "")],
            "Notification": [command_hook("blocked", "permission_prompt|idle_prompt|elicitation_dialog")],
            "Stop": [command_hook("done", "")],
            "SessionEnd": [command_hook("clear", "")]
        }
    });
    let settings_bytes = serde_json::to_vec_pretty(&hook_settings)
        .map_err(|err| format!("could not render Claude hook settings: {err}"))?;
    atomic_write(&settings, &settings_bytes, 0o600)?;

    let wrapper = render_wrapper(&real, &phux, &shim, &settings)?;
    atomic_write(&shim, wrapper.as_bytes(), 0o755)?;

    let manifest_value = serde_json::json!({
        "schema_version": 1,
        "real_claude": real,
        "shell": shell,
        "rc": rc,
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest_value)
        .map_err(|err| format!("could not render shim manifest: {err}"))?;
    atomic_write(&manifest, &manifest_bytes, 0o600)?;

    let activation = shell_activation(shell, &shim_dir)?;
    install_rc_block(&rc, &activation)?;

    Ok(InstallReport { shim, rc })
}

fn uninstall_claude() -> Result<Option<PathBuf>, String> {
    let home = home_dir()?;
    let shim_dir = data_home(&home).join("phux").join("shims");
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

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "cannot install a shell shim because HOME is unset".to_owned())
}

fn data_home(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".local").join("share"), PathBuf::from)
}

fn shell_rc(shell: &str, home: &Path) -> Result<PathBuf, String> {
    match shell {
        "zsh" => Ok(std::env::var_os("ZDOTDIR")
            .filter(|value| !value.is_empty())
            .map_or_else(
                || home.join(".zshrc"),
                |dir| PathBuf::from(dir).join(".zshrc"),
            )),
        "bash" => Ok(home.join(".bashrc")),
        "fish" => Ok(std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map_or_else(|| home.join(".config"), PathBuf::from)
            .join("fish")
            .join("config.fish")),
        other => Err(format!(
            "unsupported shell '{other}' (expected zsh, bash, or fish)"
        )),
    }
}

fn shell_activation(shell: &str, shim_dir: &Path) -> Result<String, String> {
    let path = sh_quote_path(shim_dir);
    match shell {
        "zsh" | "bash" => Ok(format!("export PATH={path}:\"$PATH\"")),
        "fish" => Ok(format!("fish_add_path --prepend {path}")),
        other => Err(format!(
            "unsupported shell '{other}' (expected zsh, bash, or fish)"
        )),
    }
}

fn resolve_real_claude(
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

fn validate_executable(path: &Path, shim_dir: &Path) -> Result<PathBuf, String> {
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

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn read_manifest(path: &Path) -> Result<Option<serde_json::Value>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| format!("could not parse {}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("could not read {}: {err}", path.display())),
    }
}

fn render_wrapper(
    real: &Path,
    phux: &Path,
    shim: &Path,
    settings: &Path,
) -> Result<String, String> {
    for path in [real, phux, shim, settings] {
        if path.as_os_str().to_string_lossy().contains(['\n', '\r']) {
            return Err(format!("path contains a newline: {}", path.display()));
        }
    }
    Ok(format!(
        r#"#!/bin/sh
set -u

real={real}
phux=${{PHUX_AGENT_PHUX_BIN:-{phux}}}
shim={shim}
settings={settings}

run_phux() {{
  "$phux" "$@" >/dev/null 2>&1 || true
}}

set_state() {{
  state=$1
  [ -n "${{PHUX_TERMINAL_ID:-}}" ] || return 0
  target="@$PHUX_TERMINAL_ID"
  case "$state" in
    clear) run_phux agent clear "$target" ;;
    blocked)
      run_phux agent set "$target" --name claude --kind claude --state blocked --attention high
      run_phux ask "$target" "Claude needs attention"
      ;;
    done) run_phux agent set "$target" --name claude --kind claude --state done --attention normal ;;
    idle) run_phux agent set "$target" --name claude --kind claude --state idle --attention none ;;
    working) run_phux agent set "$target" --name claude --kind claude --state working --attention none ;;
  esac
}}

if [ "${{1:-}}" = "--phux-hook" ]; then
  [ "$#" -eq 2 ] || exit 2
  set_state "$2"
  exit 0
fi

inner=false
if [ "${{1:-}}" = "--phux-inner" ]; then
  inner=true
  shift
fi

passthrough=false
case "${{1:-}}" in
  agents|auth|auto-mode|doctor|gateway|install|mcp|plugin|plugins|project|setup-token|update|upgrade|ultrareview) passthrough=true ;;
esac
for arg in "$@"; do
  case "$arg" in
    -p|--print|-v|--version|-h|--help|--bare|--safe-mode) passthrough=true ;;
  esac
done
if [ "$inner" = false ] && {{ [ "$passthrough" = true ] || [ ! -t 0 ] || [ ! -t 1 ]; }}; then
  exec "$real" "$@"
fi

if [ "$inner" = true ] || [ -n "${{PHUX_TERMINAL_ID:-}}" ]; then
  set_state working
  cleanup() {{ set_state clear; }}
  trap 'cleanup' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  status=0
  "$real" --settings "$settings" "$@" || status=$?
  exit "$status"
fi

cwd=$(pwd -P)
"$phux" new -c "$cwd" -- "$shim" --phux-inner "$@" && exit 0
status=$?
printf 'claude-in-phux: phux launch failed (exit %s); running Claude directly\n' "$status" >&2
exec "$real" "$@"
"#,
        real = sh_quote_path(real),
        phux = sh_quote_path(phux),
        shim = sh_quote_path(shim),
        settings = sh_quote_path(settings),
    ))
}

fn sh_quote_path(path: &Path) -> String {
    let value = path.as_os_str().to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn install_rc_block(rc: &Path, activation: &str) -> Result<(), String> {
    let existing = match std::fs::read_to_string(rc) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("could not read {}: {err}", rc.display())),
    };
    let mut updated = without_managed_block(&existing)?;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(BLOCK_BEGIN);
    updated.push('\n');
    updated.push_str(activation);
    updated.push('\n');
    updated.push_str(BLOCK_END);
    updated.push('\n');
    write_rc(rc, updated.as_bytes())
}

fn remove_rc_block(rc: &Path) -> Result<(), String> {
    let existing = match std::fs::read_to_string(rc) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(format!("could not read {}: {err}", rc.display())),
    };
    let updated = without_managed_block(&existing)?;
    if updated != existing {
        write_rc(rc, updated.as_bytes())?;
    }
    Ok(())
}

fn without_managed_block(contents: &str) -> Result<String, String> {
    let Some(start) = contents.find(BLOCK_BEGIN) else {
        return Ok(contents.to_owned());
    };
    let relative_end = contents[start..]
        .find(BLOCK_END)
        .ok_or_else(|| format!("found '{BLOCK_BEGIN}' without matching '{BLOCK_END}'"))?;
    let mut end = start + relative_end + BLOCK_END.len();
    if contents.as_bytes().get(end) == Some(&b'\r') {
        end += 1;
    }
    if contents.as_bytes().get(end) == Some(&b'\n') {
        end += 1;
    }
    let mut result = String::with_capacity(contents.len() - (end - start));
    result.push_str(&contents[..start]);
    result.push_str(&contents[end..]);
    Ok(result)
}

fn write_rc(rc: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = rc.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    let target = if std::fs::symlink_metadata(rc)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        std::fs::canonicalize(rc)
            .map_err(|err| format!("could not resolve shell rc symlink {}: {err}", rc.display()))?
    } else {
        rc.to_path_buf()
    };
    let mode = std::fs::metadata(&target)
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .unwrap_or(0o600);
    atomic_write(&target, bytes, mode)
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::{BLOCK_BEGIN, BLOCK_END, render_wrapper, shell_activation, without_managed_block};
    use std::path::Path;

    #[test]
    fn rc_block_removal_preserves_every_user_owned_byte() {
        let input = format!("before\n{BLOCK_BEGIN}\nexport PATH=x\n{BLOCK_END}\nafter\n");
        assert_eq!(without_managed_block(&input).unwrap(), "before\nafter\n");
        assert_eq!(without_managed_block("untouched\n").unwrap(), "untouched\n");
        assert!(without_managed_block(BLOCK_BEGIN).is_err());
    }

    #[test]
    fn shell_activation_quotes_paths_and_uses_native_fish_syntax() {
        let path = Path::new("/tmp/a path/it's-here");
        let quoted = "'/tmp/a path/it'\\''s-here'";
        assert_eq!(
            shell_activation("zsh", path).unwrap(),
            format!("export PATH={quoted}:\"$PATH\"")
        );
        assert_eq!(
            shell_activation("fish", path).unwrap(),
            format!("fish_add_path --prepend {quoted}")
        );
    }

    #[test]
    fn wrapper_routes_outer_interactive_claude_and_declares_inner_lifecycle() {
        let wrapper = render_wrapper(
            Path::new("/real/claude"),
            Path::new("/bin/phux"),
            Path::new("/data/phux/shims/claude"),
            Path::new("/data/phux/shims/claude-hooks.json"),
        )
        .unwrap();
        assert!(wrapper.contains("\"$phux\" new -c \"$cwd\" -- \"$shim\" --phux-inner"));
        assert!(
            wrapper.contains("agent set \"$target\" --name claude --kind claude --state blocked")
        );
        assert!(wrapper.contains("run_phux ask \"$target\" \"Claude needs attention\""));
        assert!(wrapper.contains("\"$real\" --settings \"$settings\" \"$@\""));
        assert!(wrapper.contains("run_phux agent clear \"$target\""));
    }
}
