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

/// Behavioral version of the generated wrapper, stamped into the script
/// itself so an installed copy can be recognized as stale.
///
/// The wrapper lives on disk in a phux-owned directory and keeps running
/// whatever text it was written with, so upgrading the phux binary does NOT
/// upgrade an installed shim. Without a stamp, `install-claude` cannot tell
/// "already current" from "silently running last release's behavior", and the
/// two are not cosmetically different:
///
/// * **1** — declared a real `state` (`working`/`blocked`/`done`/`idle`) plus
///   an `attention` on every Claude lifecycle hook. Per `docs/spec/L3.md`
///   §3.7 an explicit `state` outranks the server's derivation, so every pane
///   running this shim stood the detector down permanently (phux-w7z2.26) and
///   armed the wedge in phux-w7z2.13.
/// * **2** — declares identity only (`--name`/`--kind`, no `--state`, no
///   `--attention`), but still on every hook. `phux agent set` with no
///   `--state` writes the literal `"unknown"`, which is explicitly NOT a
///   declaration, so the detector kept deriving. It also replaces the record
///   WHOLESALE, so each hook clobbered the derived `state` back to `unknown`
///   and published a `working -> unknown` edge at the end of every turn —
///   which `agent wait` reads as the agent departing (phux-w7z2.37). This
///   schema traded a permanent declaration for a per-turn clobber.
/// * **3** — writes identity exactly ONCE, at session start. `blocked` still
///   fires per hook but reaches only `phux ask` (ADR-0035/0036), which writes
///   nothing to the record. Hooks that would now write nothing are not wired
///   at all.
const SHIM_SCHEMA: u32 = 3;

/// Prefix of the wrapper's schema stamp line. A `#` comment, so it is inert
/// to `/bin/sh` and greppable without executing anything.
const SCHEMA_MARKER: &str = "# phux-shim-schema: ";

pub(super) fn run_install_claude(shell: Option<&str>, real: Option<&Path>) -> ExitCode {
    let shell = shell.map_or_else(detected_shell, str::to_owned);
    match install_claude(&shell, real) {
        Ok(report) => {
            let shim = report.shim.display();
            match report.replaced {
                None => outln!("installed claude-in-phux shim at {shim}"),
                Some(prior) if prior >= SHIM_SCHEMA => {
                    outln!("reinstalled claude-in-phux shim at {shim} (schema {SHIM_SCHEMA})");
                }
                Some(prior) => {
                    outln!(
                        "upgraded claude-in-phux shim at {shim} (schema {prior} -> {SHIM_SCHEMA})"
                    );
                    let was = if prior <= 1 {
                        "declared an agent state on every Claude hook, which stood the \
                         server-side detector down for the whole session"
                    } else {
                        "rewrote the agent record on every Claude hook, which reset the \
                         detected state at the end of every turn and made `phux agent wait` \
                         report the agent as departed"
                    };
                    outln!(
                        "schema {prior} {was}; a Claude already running picks the new shim up \
                         on its next session"
                    );
                }
            }
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
    /// Schema of the shim this install replaced, if one was already on disk.
    /// `None` on a first install.
    replaced: Option<u32>,
}

fn install_claude(shell: &str, explicit_real: Option<&Path>) -> Result<InstallReport, String> {
    let home = home_dir()?;
    let shim_dir = data_home(&home).join("phux").join("shims");
    let rc = shell_rc(shell, &home)?;
    let phux = std::env::current_exe()
        .map_err(|err| format!("could not resolve the running phux binary: {err}"))?;
    install_claude_into(&shim_dir, &rc, shell, explicit_real, &phux)
}

/// The whole of `install_claude` with every ambient path handed in.
///
/// `install_claude` resolves `shim_dir` / `rc` / `phux` from the environment;
/// this takes them, so the round-trip can be tested in a tempdir without
/// mutating the process environment (`env::set_var` is unsafe under edition
/// 2024 and this crate forbids `unsafe`).
fn install_claude_into(
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

    let hook_command = |state: &str| format!("{} --phux-hook {state}", sh_quote_path(&shim));
    let command_hook = |state: &str, matcher: &str| {
        serde_json::json!({
            "matcher": matcher,
            "hooks": [{ "type": "command", "command": hook_command(state) }]
        })
    };
    let hook_settings = serde_json::json!({
        "hooks": {
            "SessionStart": [command_hook("start", "")],
            "PermissionRequest": [command_hook("blocked", "")],
            "Notification": [command_hook("blocked", "permission_prompt|idle_prompt|elicitation_dialog")],
            "SessionEnd": [command_hook("clear", "")]
        }
    });
    let settings_bytes = serde_json::to_vec_pretty(&hook_settings)
        .map_err(|err| format!("could not render Claude hook settings: {err}"))?;
    atomic_write(&settings, &settings_bytes, 0o600)?;

    let wrapper = render_wrapper(&real, phux, &shim, &settings)?;
    atomic_write(&shim, wrapper.as_bytes(), 0o755)?;

    // `schema_version` versions the MANIFEST's own shape (bumped because
    // `shim_schema` joins it); `shim_schema` versions the wrapper's behavior.
    // Both readers below (`resolve_real_claude`, `uninstall_claude_from`) pull
    // single keys and ignore the rest, so a v1 manifest still uninstalls.
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

fn uninstall_claude() -> Result<Option<PathBuf>, String> {
    let home = home_dir()?;
    let shim_dir = data_home(&home).join("phux").join("shims");
    uninstall_claude_from(&shim_dir)
}

/// `uninstall_claude` with the shim directory handed in — see
/// [`install_claude_into`] for why the seam exists.
///
/// Removes exactly the three files [`install_claude_into`] writes plus the
/// marked rc block, and nothing else in the directory.
fn uninstall_claude_from(shim_dir: &Path) -> Result<Option<PathBuf>, String> {
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

/// Render the `/bin/sh` wrapper installed as `claude`.
///
/// Its `set_state` announces WHO occupies the pane and never WHAT that
/// occupant is doing. An explicit `state` outranks the server's derivation for
/// the life of the record (`docs/spec/L3.md` §3.7), so the schema-1 wrapper —
/// which declared one on every lifecycle hook — stood the detector down on the
/// very integration phux ships its deepest manifest for, and left a dead
/// Claude wearing a live `working` badge with no path back to truth
/// (phux-w7z2.26, phux-w7z2.13). Identity-only leaves the detector deriving
/// `state` around the name and kind, which is the useful half of the feature.
///
/// The `phux ask` call on the blocked hook is deliberately kept: it feeds the
/// attention ladder (ADR-0035/0036), a path the screen cannot reconstruct and
/// one the record arbitration never touched.
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
{marker}{schema}
set -u

real={real}
phux=${{PHUX_AGENT_PHUX_BIN:-{phux}}}
shim={shim}
settings={settings}

run_phux() {{
  "$phux" "$@" >/dev/null 2>&1 || true
}}

# Identity only and exactly once; `blocked` only asks. See SHIM_SCHEMA 2 -> 3.
set_state() {{
  state=$1
  [ -n "${{PHUX_TERMINAL_ID:-}}" ] || return 0
  target="@$PHUX_TERMINAL_ID"
  case "$state" in
    clear) run_phux agent clear "$target" ;;
    start) run_phux agent set "$target" --name claude --kind claude ;;
    blocked) run_phux ask "$target" "Claude needs attention" ;;
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
  # First arg after the flag is the launch sentinel: stamping it tells the
  # outer wrapper the phux session really started, so a later nonzero exit
  # must not be treated as a launch failure.
  if [ "$#" -ge 1 ]; then
    printf started > "$1" 2>/dev/null || true
    shift
  fi
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
  set_state start
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
# The sentinel distinguishes "phux never launched the session" (safe to run
# Claude directly) from "the session started and later died" — where a
# silent relaunch would re-run the original argv (-c/--resume) in a fresh
# unhooked Claude without the user noticing. /dev/null fallback: stamping
# it is a no-op, so a broken mktemp degrades to the old always-fallback.
marker=$(mktemp 2>/dev/null) || marker=/dev/null
"$phux" new -c "$cwd" -- "$shim" --phux-inner "$marker" "$@" && {{
  [ "$marker" = /dev/null ] || rm -f "$marker"
  exit 0
}}
status=$?
started=false
[ -s "$marker" ] && started=true
[ "$marker" = /dev/null ] || rm -f "$marker"
if [ "$started" = true ]; then
  printf 'claude-in-phux: phux session ended abnormally (exit %s); not relaunching Claude\n' "$status" >&2
  exit "$status"
fi
printf 'claude-in-phux: phux launch failed (exit %s); running Claude directly\n' "$status" >&2
exec "$real" "$@"
"#,
        marker = SCHEMA_MARKER,
        schema = SHIM_SCHEMA,
        real = sh_quote_path(real),
        phux = sh_quote_path(phux),
        shim = sh_quote_path(shim),
        settings = sh_quote_path(settings),
    ))
}

/// The schema of the shim already on disk, or `None` when none is installed.
///
/// A wrapper written before stamping existed carries no marker line; it is
/// reported as schema **1**, which is exactly what it is — the declaring
/// version. Any file we cannot read is treated as absent: install overwrites
/// it either way, and the only thing riding on this is the message.
fn installed_shim_schema(shim: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(shim).ok()?;
    Some(
        text.lines()
            .find_map(|line| line.strip_prefix(SCHEMA_MARKER))
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(1),
    )
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
    use super::{
        BLOCK_BEGIN, BLOCK_END, SCHEMA_MARKER, SHIM_SCHEMA, install_claude_into, install_rc_block,
        installed_shim_schema, render_wrapper, sh_quote_path, shell_activation,
        uninstall_claude_from, without_managed_block,
    };
    use std::os::unix::fs::PermissionsExt as _;
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

    /// phux-w7z2.26. The wrapper announces WHO occupies the pane and never
    /// WHAT it is doing.
    ///
    /// This test used to assert the opposite (`--state blocked` on the
    /// blocked hook, and a declared state on each of the other three). That
    /// assertion WAS the bug: `docs/spec/L3.md` §3.7 makes an explicit
    /// `state` outrank the server's derivation for the lifetime of the
    /// record, so a shim declaring one on every hook stood the detector down
    /// on every pane running it — permanently, since nothing but
    /// `DELETE_METADATA` or pane reap withdraws a declaration. phux shipped
    /// its deepest detection manifest (`rules/claude.toml`) and its own
    /// integration disarmed it.
    ///
    /// `phux agent set` with no `--state` writes the literal `"unknown"`,
    /// which `AgentRecordArbiter::note_explicit_set` deliberately does not
    /// count as a declaration, so the detector keeps deriving `state` around
    /// the name and kind below.
    #[test]
    fn wrapper_routes_outer_interactive_claude_and_declares_inner_identity_only() {
        let wrapper = render_wrapper(
            Path::new("/real/claude"),
            Path::new("/bin/phux"),
            Path::new("/data/phux/shims/claude"),
            Path::new("/data/phux/shims/claude-hooks.json"),
        )
        .unwrap();
        assert!(wrapper.contains("\"$phux\" new -c \"$cwd\" -- \"$shim\" --phux-inner"));
        assert!(wrapper.contains("agent set \"$target\" --name claude --kind claude"));
        assert!(
            !wrapper.contains("--state"),
            "a declared state stands the detector down (w7z2.26):\n{wrapper}"
        );
        assert!(
            !wrapper.contains("--attention"),
            "attention derives from state (L3.md 3.7); declaring one pins a badge:\n{wrapper}"
        );
        // The `ask` ladder (ADR-0035/0036) is a separate path from the
        // record and must survive: it is the only thing the blocked hook
        // still contributes that the screen cannot see for itself.
        assert!(wrapper.contains("run_phux ask \"$target\" \"Claude needs attention\""));
        assert!(wrapper.contains("\"$real\" --settings \"$settings\" \"$@\""));
        assert!(wrapper.contains("run_phux agent clear \"$target\""));
    }

    /// Every lifecycle hook still reaches `set_state`; what changed is what
    /// `set_state` writes. Guards against "fixing" w7z2.26 by unwiring the
    /// hooks, which would also take the `clear` on `SessionEnd` and the
    /// `ask` on `blocked` with it.
    #[test]
    fn every_lifecycle_hook_the_installer_wires_is_still_handled() {
        let wrapper = render_wrapper(
            Path::new("/real/claude"),
            Path::new("/bin/phux"),
            Path::new("/data/phux/shims/claude"),
            Path::new("/data/phux/shims/claude-hooks.json"),
        )
        .unwrap();
        for state in ["clear", "start", "blocked"] {
            assert!(
                wrapper.contains(state),
                "hook state `{state}` is wired by the installer but unhandled by set_state",
            );
        }
        assert!(wrapper.contains("run_phux agent set \"$target\""));
    }

    /// w7z2.37: the record is written exactly ONCE, at `start`.
    ///
    /// `SET_METADATA` replaces the record wholesale, so an identity write
    /// carries `state: "unknown"`. Repeating it per hook published a
    /// `working -> unknown` edge at the end of every turn, which `agent wait`
    /// reads as the agent departing and exits `1` on — breaking the flagship
    /// orchestration loop on exactly the panes phux instruments most deeply.
    /// The first `w7z2.26` fix made the shim identity-only but left the write
    /// on every hook, so it traded a permanent declaration for a per-turn
    /// clobber. CI was green with that bug in place.
    ///
    /// This asserts the shape structurally: only the `start` arm may reach
    /// `agent set`, and `blocked` — which still fires per hook — must reach
    /// `ask` and nothing else.
    #[test]
    fn only_the_start_arm_writes_the_record() {
        let wrapper = render_wrapper(
            Path::new("/real/claude"),
            Path::new("/bin/phux"),
            Path::new("/data/phux/shims/claude"),
            Path::new("/data/phux/shims/claude-hooks.json"),
        )
        .unwrap();

        let writes: Vec<&str> = wrapper
            .lines()
            .filter(|line| line.contains("agent set \"$target\""))
            .collect();
        assert_eq!(
            writes.len(),
            1,
            "the record must be written exactly once, from the `start` arm; found:\n{writes:#?}"
        );
        assert!(
            writes[0].trim_start().starts_with("start)"),
            "the sole record write must be the `start` arm, not a per-turn hook: {}",
            writes[0]
        );

        let blocked = wrapper
            .lines()
            .find(|line| line.trim_start().starts_with("blocked)"))
            .expect("a blocked arm");
        assert!(
            !blocked.contains("agent set"),
            "blocked fires per hook, so a record write there is the w7z2.37 clobber: {blocked}"
        );
        assert!(
            blocked.contains("run_phux ask"),
            "blocked must still ask: {blocked}"
        );

        // A per-turn hook that writes nothing should not be wired at all: it
        // would spawn a subprocess per turn to no effect.
        for hook in ["UserPromptSubmit", "Stop"] {
            assert!(
                !wrapper.contains(hook),
                "`{hook}` no longer writes anything and must not be wired",
            );
        }
    }

    /// The wrapper carries its own behavioral version, so an install can tell
    /// "already current" from "still running the declaring shim".
    #[test]
    fn the_wrapper_is_schema_stamped_and_an_unstamped_one_reads_as_schema_one() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let shim = dir.path().join("claude");
        assert_eq!(installed_shim_schema(&shim), None, "nothing installed yet");

        // A pre-stamping wrapper: no marker line anywhere.
        std::fs::write(
            &shim,
            "#!/bin/sh\nset -u\nrun_phux agent set \"$target\" --name claude --state working\n",
        )
        .unwrap();
        assert_eq!(
            installed_shim_schema(&shim),
            Some(1),
            "an unstamped shim IS the declaring version",
        );

        let rendered = render_wrapper(
            Path::new("/real/claude"),
            Path::new("/bin/phux"),
            &shim,
            Path::new("/data/claude-hooks.json"),
        )
        .unwrap();
        assert!(rendered.contains(&format!("{SCHEMA_MARKER}{SHIM_SCHEMA}\n")));
        std::fs::write(&shim, &rendered).unwrap();
        assert_eq!(installed_shim_schema(&shim), Some(SHIM_SCHEMA));
    }

    /// Install over a stale (schema-1) install, then uninstall, and account
    /// for every byte on both sides.
    ///
    /// The migration contract: `install-claude` REPLACES a stale shim rather
    /// than leaving a silent behavior split between installed and
    /// freshly-installed users, reports which schema it replaced, and
    /// `uninstall-claude` still removes exactly the three files the installer
    /// writes plus the marked rc block — no more, no less.
    #[test]
    fn install_over_a_stale_shim_upgrades_it_and_uninstall_removes_exactly_its_own_files() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let shim_dir = dir.path().join("shims");
        let rc = dir.path().join("rc");
        let phux = dir.path().join("phux");
        let real = dir.path().join("real-claude");
        std::fs::write(&real, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();

        // A stale install: the schema-1 wrapper, its hook settings, a v1
        // manifest (no `shim_schema` key), and the rc block.
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::write(
            shim_dir.join("claude"),
            "#!/bin/sh\nrun_phux agent set \"$target\" --name claude --state working\n",
        )
        .unwrap();
        std::fs::write(shim_dir.join("claude-hooks.json"), b"{}").unwrap();
        std::fs::write(
            shim_dir.join(super::MANIFEST),
            serde_json::json!({
                "schema_version": 1,
                "real_claude": real,
                "shell": "zsh",
                "rc": rc,
            })
            .to_string(),
        )
        .unwrap();
        // A file phux does not own, to prove uninstall does not over-reach.
        std::fs::write(shim_dir.join("keep-me"), b"not ours").unwrap();

        let user_rc = "# my rc\nalias ll='ls -l'\n";
        std::fs::write(&rc, user_rc).unwrap();
        install_rc_block(&rc, "export PATH=stale:\"$PATH\"").unwrap();

        // --- upgrade ------------------------------------------------------
        let report = install_claude_into(&shim_dir, &rc, "zsh", Some(&real), &phux).unwrap();
        assert_eq!(
            report.replaced,
            Some(1),
            "the stale shim must be recognized, not silently overwritten",
        );
        let installed = std::fs::read_to_string(shim_dir.join("claude")).unwrap();
        assert_eq!(
            installed_shim_schema(&shim_dir.join("claude")),
            Some(SHIM_SCHEMA)
        );
        assert!(
            !installed.contains("--state"),
            "the upgraded shim must not declare state:\n{installed}"
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(shim_dir.join(super::MANIFEST)).unwrap())
                .unwrap();
        assert_eq!(manifest["shim_schema"], serde_json::json!(SHIM_SCHEMA));

        // Re-installing is idempotent and leaves exactly one managed block.
        let again = install_claude_into(&shim_dir, &rc, "zsh", Some(&real), &phux).unwrap();
        assert_eq!(again.replaced, Some(SHIM_SCHEMA), "already current");
        let rc_text = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(rc_text.matches(BLOCK_BEGIN).count(), 1);
        assert!(
            rc_text.starts_with(user_rc),
            "user bytes survive: {rc_text:?}"
        );

        // --- uninstall ----------------------------------------------------
        let removed = uninstall_claude_from(&shim_dir).unwrap();
        assert_eq!(removed.as_deref(), Some(rc.as_path()));
        assert_eq!(
            std::fs::read_to_string(&rc).unwrap(),
            user_rc,
            "the rc returns byte-for-byte to what the user had",
        );
        let mut left: Vec<_> = std::fs::read_dir(&shim_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![std::ffi::OsString::from("keep-me")],
            "uninstall removes its own three files and nothing else",
        );
        assert!(uninstall_claude_from(&shim_dir).unwrap().is_none());
    }

    /// Behavioral proof for phux-t2g: a rendered wrapper, run for real
    /// (fake `phux` and fake `claude` standing in), must not relaunch
    /// Claude after the phux session already started and later died.
    ///
    /// The outer dispatch-to-`phux new` block only runs when the wrapper
    /// is invoked interactively (`[ -t 0 ] && [ -t 1 ]` — see
    /// `render_wrapper`), so this drives the wrapper on a real pty rather
    /// than asserting on the rendered text alone.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one linear pty-driven scenario")]
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "`${...}` here is shell parameter expansion in a fixture script, not a std format arg"
    )]
    fn wrapper_never_relaunches_claude_after_a_mid_session_crash() {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        use std::io::Read as _;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        fn write_script(path: &Path, body: &str) {
            std::fs::write(path, body).expect("write fixture script");
            let mut perms = std::fs::metadata(path)
                .expect("stat fixture script")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).expect("chmod fixture script");
        }

        /// Run `wrapper` attached to a real pty (both stdin and stdout,
        /// so the wrapper's tty guard sees an interactive session) and
        /// return `(exit_code, combined_stdout_and_stderr)`.
        fn run_on_pty(wrapper: &Path, envs: &[(&str, &str)]) -> (u32, String) {
            let pty = native_pty_system();
            let pair = pty
                .openpty(PtySize {
                    rows: 24,
                    cols: 100,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .expect("open test pty");
            let mut command = CommandBuilder::new(wrapper);
            command.env("SHELL", "/bin/sh");
            command.env("TERM", "xterm-256color");
            for (key, value) in envs {
                command.env(key, value);
            }
            let mut child = pair
                .slave
                .spawn_command(command)
                .expect("spawn wrapper under pty");
            drop(pair.slave);

            let output = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&output);
            let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
            std::thread::spawn(move || {
                let mut buf = [0_u8; 8192];
                while let Ok(read) = reader.read(&mut buf) {
                    if read == 0 {
                        break;
                    }
                    sink.lock()
                        .expect("output lock")
                        .extend_from_slice(&buf[..read]);
                }
            });
            drop(pair.master);

            let deadline = Instant::now() + Duration::from_secs(10);
            let status = loop {
                if let Some(status) = child.try_wait().expect("wrapper try_wait") {
                    break status;
                }
                assert!(
                    Instant::now() < deadline,
                    "wrapper did not exit within the deadline"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            // Let the reader thread drain whatever is left in the pty buffer.
            std::thread::sleep(Duration::from_millis(50));
            let text = String::from_utf8_lossy(&output.lock().expect("output lock")).into_owned();
            (status.exit_code(), text)
        }

        let dir = tempfile::tempdir().expect("scratch dir");
        let real = dir.path().join("real-claude");
        let fake_phux = dir.path().join("fake-phux");
        let wrapper = dir.path().join("claude");
        let settings = dir.path().join("claude-hooks.json");
        let log = dir.path().join("real-claude.log");

        // Stands in for the real `claude`: records that it ran, then exits
        // with a caller-controlled status (simulating either a clean exit
        // or Claude itself dying mid-session).
        write_script(
            &real,
            &format!(
                "#!/bin/sh\nprintf 'invoked\\n' >> {log}\nexit \"${{FAKE_CLAUDE_EXIT:-0}}\"\n",
                log = sh_quote_path(&log),
            ),
        );
        // Stands in for `phux new -c <cwd> -- <cmd...>`: either fails
        // outright before ever running the session command (simulating a
        // launch that never started), or execs the given command exactly
        // like the real subcommand would.
        write_script(
            &fake_phux,
            "#!/bin/sh\nset -u\nif [ \"${FAKE_PHUX_LAUNCH_FAIL:-0}\" = \"1\" ]; then\n  exit 3\nfi\nshift 4\nexec \"$@\"\n",
        );
        let rendered = render_wrapper(&real, &fake_phux, &wrapper, &settings).unwrap();
        write_script(&wrapper, &rendered);

        // Scenario A: the session runs and Claude exits cleanly.
        std::fs::write(&log, b"").unwrap();
        let (status, output) = run_on_pty(&wrapper, &[("FAKE_CLAUDE_EXIT", "0")]);
        assert_eq!(status, 0, "clean session exit; output:\n{output}");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count(),
            1,
            "Claude must run exactly once; output:\n{output}"
        );
        assert!(!output.contains("abnormally"), "{output}");
        assert!(!output.contains("launch failed"), "{output}");

        // Scenario B (the phux-t2g bug): the session starts, then Claude
        // (or the server under it) dies mid-run. The old wrapper treated
        // ANY nonzero `phux new` exit as a launch failure and silently
        // re-exec'd a fresh, unhooked Claude with the original argv. The
        // fix must propagate the real exit status and must NOT invoke
        // Claude a second time.
        std::fs::write(&log, b"").unwrap();
        let (status, output) = run_on_pty(&wrapper, &[("FAKE_CLAUDE_EXIT", "17")]);
        assert_eq!(
            status, 17,
            "mid-session crash status must propagate; output:\n{output}"
        );
        assert!(output.contains("phux session ended abnormally"), "{output}");
        assert!(!output.contains("launch failed"), "{output}");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count(),
            1,
            "Claude must not be relaunched after the session already started; output:\n{output}"
        );

        // Scenario C: `phux new` fails before the session ever starts (no
        // marker stamped) — falling back to a direct, real Claude exactly
        // once is still correct here.
        std::fs::write(&log, b"").unwrap();
        let (status, output) = run_on_pty(
            &wrapper,
            &[("FAKE_PHUX_LAUNCH_FAIL", "1"), ("FAKE_CLAUDE_EXIT", "0")],
        );
        assert_eq!(
            status, 0,
            "pre-launch failure falls back to direct Claude; output:\n{output}"
        );
        assert!(output.contains("phux launch failed"), "{output}");
        assert!(!output.contains("abnormally"), "{output}");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count(),
            1,
            "the direct fallback must still run Claude exactly once; output:\n{output}"
        );
    }
}
