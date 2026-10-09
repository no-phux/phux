//! Install the `claude` adoption shim: plain `claude` becomes a phux session.
//!
//! The shim is deliberately installed into a phux-owned directory and activated
//! by one bounded shell-rc block. It never overwrites the real Claude binary.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::companion::is_executable;

const BLOCK_BEGIN: &str = "# >>> phux agent shims >>>";
const BLOCK_END: &str = "# <<< phux agent shims <<<";
const MANIFEST: &str = "claude-install.json";

/// Behavioral version of the generated wrapper, stamped into the script so
/// an installed copy (which a binary upgrade does not rewrite) can be
/// recognized as stale by `install-claude` and `phux doctor`.
///
/// 1 declared a `state` on every hook (standing the detector down); 2 wrote
/// identity on every hook (clobbering derived state each turn); 3 writes
/// identity once at session start; 4 feeds per-turn edges through
/// `report-state`; 5 reads the hook payload and, when the server serves
/// `AgentSession` resources, feeds a session stream with `agent emit`; 6 adds
/// the `phux.transcript/v1` entries for prompts, tool results, and replies.
pub(crate) const SHIM_SCHEMA: u32 = 6;

/// Prefix of the wrapper's schema stamp line. A `#` comment, so it is inert
/// to `/bin/sh` and greppable without executing anything.
const SCHEMA_MARKER: &str = "# phux-shim-schema: ";

/// The Claude Code hooks the installer registers: `(event, matcher, arm)`,
/// each becoming `<shim> --phux-hook <arm>`. Every arm must be handled by the
/// wrapper's `stream_state`.
const HOOKS: &[(&str, &str, &str)] = &[
    ("SessionStart", "", "start"),
    ("UserPromptSubmit", "", "working"),
    ("PreToolUse", "", "tool-start"),
    ("PostToolUse", "", "tool-end"),
    ("PermissionRequest", "", "blocked"),
    (
        "Notification",
        "permission_prompt|idle_prompt|elicitation_dialog",
        "blocked",
    ),
    ("Stop", "", "done"),
    ("SessionEnd", "", "clear"),
];

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
                    } else if prior == 2 {
                        "rewrote the agent record on every Claude hook, which reset the \
                         detected state at the end of every turn and made `phux agent wait` \
                         report the agent as departed"
                    } else if prior == 3 {
                        "left lifecycle timing to screen detection and could not publish \
                         the Claude Stop hook's exact `done` edge"
                    } else if prior == 4 {
                        "reported lifecycle edges to the detector but never read the hook \
                         payload, so it could not open or feed the pane's agent session \
                         stream"
                    } else {
                        "fed the agent session stream lifecycle records only, so phones \
                         and other stream readers could not show the conversation"
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

/// Where the wrapper installed as `claude` lives for this user.
fn shim_dir_for(home: &Path) -> PathBuf {
    data_home(home).join("phux").join("shims")
}

/// The installed wrapper's path (for `phux doctor`), or `None` without `HOME`.
pub(crate) fn installed_shim_path() -> Option<PathBuf> {
    Some(shim_dir_for(&home_dir().ok()?).join("claude"))
}

fn install_claude(shell: &str, explicit_real: Option<&Path>) -> Result<InstallReport, String> {
    let home = home_dir()?;
    let shim_dir = shim_dir_for(&home);
    let rc = shell_rc(shell, &home)?;
    let phux = std::env::current_exe()
        .map_err(|err| format!("could not resolve the running phux binary: {err}"))?;
    install_claude_into(&shim_dir, &rc, shell, explicit_real, &phux)
}

/// `install_claude` with every ambient path handed in, so tests can run it
/// in a tempdir without mutating the process environment.
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

    let hook_command = |arm: &str| format!("{} --phux-hook {arm}", sh_quote_path(&shim));
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

fn uninstall_claude() -> Result<Option<PathBuf>, String> {
    let shim_dir = shim_dir_for(&home_dir()?);
    uninstall_claude_from(&shim_dir)
}

/// `uninstall_claude` with the shim directory handed in. Removes exactly the
/// three files [`install_claude_into`] writes plus the marked rc block.
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
/// Every `--phux-hook` arm reads Claude's stdin payload through
/// `phux agent hook-payload`, then: `start` writes identity once (never a
/// `--state`, which would outrank the server's derivation, L3.md §3.7);
/// the lifecycle arms feed a session stream with `agent emit` when
/// `phux status --json` lists `resource_kinds` (probed once per process),
/// else `report-state` (ADR-0085); `blocked` always calls `phux ask` and
/// `clear` always deletes the record. A session is opened at most once per
/// process and never for a `compact`-sourced `SessionStart`.
///
/// Payload text never reaches a command line: only the tokens `hook-payload`
/// prints (never prompt text, `tool_input`, `tool_response`, or the
/// transcript) do. The `working`, `tool-end`, and `done` arms append the
/// `phux.transcript/v1` entry `phux agent hook-transcript` prints, fed on
/// stdin, unless `PHUX_AGENT_TRANSCRIPT=0` (ADR-0156); the pane's scrollback
/// already shows that text under the same access control.
/// `PHUX_AGENT_EMIT_RAW=1` opts the whole payload into a `provider_raw`
/// record, fed on stdin.
#[allow(
    clippy::too_many_lines,
    reason = "one shell script, rendered as one literal so it reads as the script it is"
)]
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

# Does the server serve AgentSession resources? `phux status --json` lists
# `resource_kinds` under `features` when it does. Probed at most once per
# wrapper process: the launching wrapper lives for the whole Claude session,
# and each hook is its own short process.
streams=
server_streams() {{
  if [ -z "$streams" ]; then
    streams=no
    case $("$phux" status --json 2>/dev/null) in
      *'"resource_kinds"'*) streams=yes ;;
    esac
  fi
  [ "$streams" = yes ]
}}

# Fields of the hook payload on stdin, exactly as `phux agent hook-payload`
# prints them: one line of shell-safe tokens, `-` when absent. Callers with
# no payload (the launch path, the exit trap) see every field absent.
hook_session=-
hook_event=-
hook_tool=-
hook_kind=-
hook_chars=0
hook_reason=-
hook_source=-
payload=
read_payload() {{
  [ ! -t 0 ] || return 0
  payload=$(mktemp 2>/dev/null) || {{ payload=; return 0; }}
  cat > "$payload" 2>/dev/null || :
  fields=$("$phux" agent hook-payload < "$payload" 2>/dev/null) || fields=
  [ -n "$fields" ] || return 0
  # shellcheck disable=SC2086 # the helper prints tokens with no IFS or glob characters
  set -- $fields
  [ "$#" -eq 7 ] || return 0
  hook_session=$1
  hook_event=$2
  hook_tool=$3
  hook_kind=$4
  hook_chars=$5
  hook_reason=$6
  hook_source=$7
  case "$hook_chars" in *[!0-9]*) hook_chars=0 ;; esac
}}

emit() {{
  if [ "$#" -gt 1 ]; then
    run_phux agent emit "$target" --type "$1" --data "$2"
  else
    run_phux agent emit "$target" --type "$1"
  fi
}}

emit_tool() {{
  if [ "$hook_tool" != - ]; then
    emit "$1" "{{\"tool_name\":\"$hook_tool\"}}"
  else
    emit "$1"
  fi
}}

# The whole hook payload, only when the user opted in. Never after
# `session_end`: the `clear` arm calls this before it ends the session.
emit_raw() {{
  if [ "${{PHUX_AGENT_EMIT_RAW:-0}}" = 1 ] && [ -n "$payload" ] && [ -s "$payload" ]; then
    run_phux agent emit "$target" --type provider_raw --data - < "$payload"
  fi
}}

# The transcript entry for this hook (ADR-0156): the prompt, a finished tool
# call, or the turn's last reply, as `phux.transcript/v1` data that
# `phux agent hook-transcript` prints and `agent emit` reads on stdin, never
# on a command line. On by default; `PHUX_AGENT_TRANSCRIPT=0` opts out.
emit_transcript() {{
  [ "${{PHUX_AGENT_TRANSCRIPT:-1}}" != 0 ] || return 0
  [ -n "$payload" ] || return 0
  [ -s "$payload" ] || return 0
  entry=$(mktemp 2>/dev/null) || return 0
  if "$phux" agent hook-transcript < "$payload" > "$entry" 2>/dev/null && [ -s "$entry" ]; then
    run_phux agent emit "$target" --type provider_raw --data - < "$entry"
  fi
  rm -f "$entry"
}}

# The session-stream arms. Typed records never carry prompt text or tool
# input: `prompt` is a character count; `tool_*` name the tool. Text rides
# only the transcript entry and the raw opt-in.
opened=false
stream_state() {{
  case "$1" in
    start)
      if [ "$hook_session" != - ] && [ "$hook_source" != compact ] && [ "$opened" = false ]; then
        opened=true
        run_phux agent session open "$target" --provider claude --native-id="$hook_session"
        emit session_start
      fi
      emit_raw
      ;;
    working)
      emit prompt "{{\"chars\":$hook_chars}}"
      emit_transcript
      emit_raw
      ;;
    tool-start)
      emit_tool tool_start
      emit_raw
      ;;
    tool-end)
      emit_tool tool_end
      emit_transcript
      emit_raw
      ;;
    blocked)
      if [ "$hook_event" = Notification ]; then
        case "$hook_kind" in
          permission_prompt) kind=permission ;;
          elicitation_dialog|elicitation_url_dialog) kind=elicitation ;;
          idle_prompt) kind=idle ;;
          *) kind=$hook_kind ;;
        esac
        emit notification "{{\"kind\":\"$kind\"}}"
      else
        emit ask
      fi
      emit_raw
      ;;
    done)
      emit_transcript
      emit stop
      emit_raw
      ;;
    clear)
      emit_raw
      if [ "$hook_reason" != - ]; then
        emit session_end "{{\"reason\":\"$hook_reason\"}}"
      else
        emit session_end
      fi
      run_phux agent session close "$target"
      ;;
  esac
}}

# Servers without AgentSession resources: the per-turn arms feed the
# detector directly (ADR-0085), and the tool arms have nothing to report.
legacy_state() {{
  case "$1" in
    working|done) run_phux agent report-state "$target" "$1" ;;
    blocked) run_phux agent report-state "$target" blocked ;;
  esac
}}

set_state() {{
  [ -n "${{PHUX_TERMINAL_ID:-}}" ] || return 0
  target="@$PHUX_TERMINAL_ID"
  # Identity is written once, at start, on every server.
  case "$1" in
    start) run_phux agent set "$target" --name claude --kind claude ;;
  esac
  if server_streams; then
    stream_state "$1"
  else
    legacy_state "$1"
  fi
  # The attention ladder and the record cleanup run on every server.
  case "$1" in
    blocked) run_phux ask "$target" "Claude needs attention" ;;
    clear) run_phux agent clear "$target" ;;
  esac
}}

if [ "${{1:-}}" = "--phux-hook" ]; then
  [ "$#" -eq 2 ] || exit 2
  read_payload
  set_state "$2"
  [ -z "$payload" ] || rm -f "$payload"
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
  # Runs once: INT/TERM/HUP re-enter through EXIT, and `session_end` is
  # terminal for the stream this process may have opened.
  ended=false
  # shellcheck disable=SC2329 # invoked through the EXIT trap below
  cleanup() {{
    [ "$ended" = false ] || return 0
    ended=true
    set_state clear
  }}
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
    let mode =
        std::fs::metadata(&target).map_or(0o600, |metadata| metadata.permissions().mode() & 0o777);
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
        BLOCK_BEGIN, BLOCK_END, HOOKS, SHIM_SCHEMA, install_claude_into, install_rc_block,
        installed_shim_schema, render_wrapper, sh_quote_path, shell_activation,
        uninstall_claude_from, without_managed_block,
    };
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    /// The wrapper with fixed placeholder paths, for the text assertions.
    fn rendered() -> String {
        render_wrapper(
            Path::new("/real/claude"),
            Path::new("/bin/phux"),
            Path::new("/data/phux/shims/claude"),
            Path::new("/data/phux/shims/claude-hooks.json"),
        )
        .unwrap()
    }

    /// A fake `phux` that logs every argv line to `$FAKE_LOG`, answers the
    /// capability probe from `$FAKE_FEATURES`, stands in for
    /// `agent hook-payload` with the canned `$FAKE_FIELDS` line (the real
    /// helper is pinned by `hook_payload.rs`), and logs whatever an
    /// `emit --data -` fed it on stdin — so a test can see exactly which
    /// bytes of a hook payload ever reached a `phux` process.
    const FAKE_PHUX: &str = concat!(
        "#!/bin/sh\n",
        "printf '%s\\n' \"$*\" >> \"$FAKE_LOG\"\n",
        "case \"$1 ${2:-}\" in\n",
        "  \"status --json\") printf '{\"running\":true,\"features\":%s}\\n' \"$FAKE_FEATURES\"; exit 0 ;;\n",
        "  \"agent hook-payload\") cat > /dev/null; printf '%s\\n' \"$FAKE_FIELDS\"; exit 0 ;;\n",
        "  \"agent hook-transcript\") cat > /dev/null; [ -z \"${FAKE_TRANSCRIPT:-}\" ] || printf '%s\\n' \"$FAKE_TRANSCRIPT\"; exit 0 ;;\n",
        "esac\n",
        "case \"$*\" in *\"--data -\") printf 'stdin:%s\\n' \"$(cat)\" >> \"$FAKE_LOG\" ;; esac\n",
        "exit 0\n",
    );

    /// A rendered wrapper plus the fake `phux` it dispatches to, on disk.
    struct Harness {
        _dir: tempfile::TempDir,
        wrapper: std::path::PathBuf,
        log: std::path::PathBuf,
    }

    impl Harness {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("scratch dir");
            let fake_phux = dir.path().join("fake-phux");
            let wrapper = dir.path().join("claude");
            let settings = dir.path().join("claude-hooks.json");
            std::fs::write(&fake_phux, FAKE_PHUX).unwrap();
            std::fs::set_permissions(&fake_phux, std::fs::Permissions::from_mode(0o755)).unwrap();
            let text =
                render_wrapper(Path::new("/real/claude"), &fake_phux, &wrapper, &settings).unwrap();
            std::fs::write(&wrapper, text).unwrap();
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self {
                _dir: dir,
                wrapper,
                log: dir_log(&fake_phux),
            }
        }

        /// Run `<wrapper> --phux-hook <arm>` with `payload` on stdin, the
        /// probe answering `features`, the helper answering `fields`, and
        /// return every `phux` argv line the fake logged, in order.
        fn hook(
            &self,
            arm: &str,
            payload: &str,
            features: &str,
            fields: &str,
            raw: bool,
        ) -> Vec<String> {
            self.hook_env(arm, payload, features, fields, raw, &[])
        }

        /// [`Self::hook`] with extra environment: `FAKE_TRANSCRIPT` is the
        /// line the fake `agent hook-transcript` prints, and
        /// `PHUX_AGENT_TRANSCRIPT` the opt-out. Both are unset otherwise.
        fn hook_env(
            &self,
            arm: &str,
            payload: &str,
            features: &str,
            fields: &str,
            raw: bool,
            env: &[(&str, &str)],
        ) -> Vec<String> {
            use std::io::Write as _;
            use std::process::{Command, Stdio};

            let _ = std::fs::remove_file(&self.log);
            let mut cmd = Command::new(&self.wrapper);
            cmd.args(["--phux-hook", arm])
                .env_remove("PHUX_AGENT_PHUX_BIN")
                .env("PHUX_TERMINAL_ID", "42")
                .env("FAKE_LOG", &self.log)
                .env("FAKE_FEATURES", features)
                .env("FAKE_FIELDS", fields)
                .env_remove("FAKE_TRANSCRIPT")
                .env_remove("PHUX_AGENT_TRANSCRIPT")
                .envs(env.iter().copied())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if raw {
                cmd.env("PHUX_AGENT_EMIT_RAW", "1");
            } else {
                cmd.env_remove("PHUX_AGENT_EMIT_RAW");
            }
            let mut child = cmd.spawn().expect("spawn the wrapper");
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(payload.as_bytes())
                .unwrap();
            let out = child.wait_with_output().expect("wrapper exit");
            assert!(
                out.status.success(),
                "hook {arm} exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                out.stdout.is_empty(),
                "a hook must print nothing to Claude: {:?}",
                String::from_utf8_lossy(&out.stdout)
            );
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    fn dir_log(fake_phux: &Path) -> std::path::PathBuf {
        fake_phux.with_file_name("fake-phux.log")
    }

    const STREAMING: &str = "[\"report_agent_state\",\"resource_kinds\"]";
    const LEGACY: &str = "[\"report_agent_state\"]";

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

    /// Every hook the installer registers has an arm in the wrapper's stream
    /// path, and the legacy path keeps its arms; identity, ask, and clear sit
    /// outside both lifecycle paths.
    #[test]
    fn every_registered_hook_arm_is_handled_and_vice_versa() {
        let wrapper = rendered();
        let stream = section(&wrapper, "stream_state() {", "legacy_state() {");
        let fallback_only = section(&wrapper, "legacy_state() {", "set_state() {");
        for (event, _, arm) in HOOKS {
            assert!(
                stream.contains(&format!("{arm})")),
                "{event} is registered as `--phux-hook {arm}` but stream_state has no `{arm})` arm",
            );
        }
        for body in [stream, fallback_only] {
            assert!(!body.contains("agent set") && !body.contains("agent clear"));
        }
        assert!(!fallback_only.contains("run_phux ask"));
    }

    /// The text of `wrapper` between the first occurrence of `from` and
    /// the next occurrence of `to`.
    fn section<'a>(wrapper: &'a str, from: &str, to: &str) -> &'a str {
        let start = wrapper
            .find(from)
            .unwrap_or_else(|| panic!("no `{from}` in wrapper"));
        let end = wrapper[start..]
            .find(to)
            .unwrap_or_else(|| panic!("no `{to}` after `{from}`"));
        &wrapper[start..start + end]
    }

    /// Against a server that serves `AgentSession` resources, each arm's
    /// exact `phux` command lines: the session is opened once with Claude's
    /// own session id, records carry only counts, tool names, and kinds,
    /// `blocked` still feeds the attention ladder, and `clear` ends and
    /// closes the session. Pinned as argv so a change here is a change to
    /// the producer contract, visibly.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one table of arms, each pinned as argv"
    )]
    fn stream_arms_emit_exactly_these_command_lines() {
        let h = Harness::new();
        let fields =
            |event: &str, tool: &str, kind: &str, chars: &str, reason: &str, source: &str| {
                format!("sess-1 {event} {tool} {kind} {chars} {reason} {source}")
            };
        let cases: Vec<(&str, String, Vec<&str>)> = vec![
            (
                "start",
                fields("SessionStart", "-", "-", "0", "-", "startup"),
                vec![
                    "agent hook-payload",
                    "agent set @42 --name claude --kind claude",
                    "status --json",
                    "agent session open @42 --provider claude --native-id=sess-1",
                    "agent emit @42 --type session_start",
                ],
            ),
            (
                "working",
                fields("UserPromptSubmit", "-", "-", "18", "-", "-"),
                vec![
                    "agent hook-payload",
                    "status --json",
                    "agent emit @42 --type prompt --data {\"chars\":18}",
                    "agent hook-transcript",
                ],
            ),
            (
                "tool-start",
                fields("PreToolUse", "Bash", "-", "0", "-", "-"),
                vec![
                    "agent hook-payload",
                    "status --json",
                    "agent emit @42 --type tool_start --data {\"tool_name\":\"Bash\"}",
                ],
            ),
            (
                "tool-end",
                fields("PostToolUse", "mcp__phux__phux_ls", "-", "0", "-", "-"),
                vec![
                    "agent hook-payload",
                    "status --json",
                    "agent emit @42 --type tool_end --data {\"tool_name\":\"mcp__phux__phux_ls\"}",
                    "agent hook-transcript",
                ],
            ),
            (
                "blocked",
                fields("PermissionRequest", "Bash", "-", "0", "-", "-"),
                vec![
                    "agent hook-payload",
                    "status --json",
                    "agent emit @42 --type ask",
                    "ask @42 Claude needs attention",
                ],
            ),
            (
                "blocked",
                fields("Notification", "-", "permission_prompt", "0", "-", "-"),
                vec![
                    "agent hook-payload",
                    "status --json",
                    "agent emit @42 --type notification --data {\"kind\":\"permission\"}",
                    "ask @42 Claude needs attention",
                ],
            ),
            (
                "blocked",
                fields("Notification", "-", "elicitation_dialog", "0", "-", "-"),
                vec![
                    "agent hook-payload",
                    "status --json",
                    "agent emit @42 --type notification --data {\"kind\":\"elicitation\"}",
                    "ask @42 Claude needs attention",
                ],
            ),
            (
                "blocked",
                fields("Notification", "-", "idle_prompt", "0", "-", "-"),
                vec![
                    "agent hook-payload",
                    "status --json",
                    "agent emit @42 --type notification --data {\"kind\":\"idle\"}",
                    "ask @42 Claude needs attention",
                ],
            ),
            (
                "done",
                fields("Stop", "-", "-", "0", "-", "-"),
                vec![
                    "agent hook-payload",
                    "status --json",
                    "agent hook-transcript",
                    "agent emit @42 --type stop",
                ],
            ),
            (
                "clear",
                fields("SessionEnd", "-", "-", "0", "prompt_input_exit", "-"),
                vec![
                    "agent hook-payload",
                    "status --json",
                    "agent emit @42 --type session_end --data {\"reason\":\"prompt_input_exit\"}",
                    "agent session close @42",
                    "agent clear @42",
                ],
            ),
        ];
        for (arm, fields, want) in cases {
            let log = h.hook(arm, "{}", STREAMING, &fields, false);
            assert_eq!(log, want, "arm `{arm}` with fields `{fields}`");
        }

        // With no session id (a payload-less start, or a helper that could
        // not parse), `start` writes identity and opens nothing: the native
        // id is the join key, and a second open would create a second
        // session.
        let log = h.hook("start", "", STREAMING, "- - - - 0 - -", false);
        assert_eq!(
            log,
            [
                "agent hook-payload",
                "agent set @42 --name claude --kind claude",
                "status --json",
            ]
        );
        // A `compact`-sourced SessionStart arrives with no SessionEnd before
        // it, so opening here would leave two live sessions under the pane.
        let log = h.hook(
            "start",
            "",
            STREAMING,
            "sess-1 SessionStart - - 0 - compact",
            false,
        );
        assert_eq!(
            log,
            [
                "agent hook-payload",
                "agent set @42 --name claude --kind claude",
                "status --json",
            ]
        );
        // `clear` still ends, closes, and clears without a reason.
        let log = h.hook("clear", "", STREAMING, "- - - - 0 - -", false);
        assert_eq!(
            log,
            [
                "agent hook-payload",
                "status --json",
                "agent emit @42 --type session_end",
                "agent session close @42",
                "agent clear @42",
            ]
        );
    }

    /// Against a server without `resource_kinds`, every arm runs exactly the
    /// schema-4 command lines — identity once, detector evidence per turn,
    /// `ask` on blocked, `clear` at the end — and the tool arms run nothing.
    /// The fallback is today's behavior, not an approximation of it.
    #[test]
    fn legacy_arms_are_the_schema_four_command_lines() {
        let h = Harness::new();
        let cases: &[(&str, &[&str])] = &[
            (
                "start",
                &[
                    "agent hook-payload",
                    "agent set @42 --name claude --kind claude",
                    "status --json",
                ],
            ),
            (
                "working",
                &[
                    "agent hook-payload",
                    "status --json",
                    "agent report-state @42 working",
                ],
            ),
            ("tool-start", &["agent hook-payload", "status --json"]),
            ("tool-end", &["agent hook-payload", "status --json"]),
            (
                "blocked",
                &[
                    "agent hook-payload",
                    "status --json",
                    "agent report-state @42 blocked",
                    "ask @42 Claude needs attention",
                ],
            ),
            (
                "done",
                &[
                    "agent hook-payload",
                    "status --json",
                    "agent report-state @42 done",
                ],
            ),
            (
                "clear",
                &["agent hook-payload", "status --json", "agent clear @42"],
            ),
        ];
        for (arm, want) in cases {
            let log = h.hook(
                arm,
                "{}",
                LEGACY,
                "sess-1 X Bash permission_prompt 9 clear startup",
                false,
            );
            assert_eq!(log, *want, "legacy arm `{arm}`");
            assert!(
                log.iter()
                    .all(|line| !line.contains("emit") && !line.contains("session")),
                "the legacy path must never touch the stream verbs: {log:?}"
            );
        }
    }

    /// No byte of the hook payload reaches a `phux` command line except
    /// through the helper's six tokens: the prompt text, `tool_input`, and
    /// `tool_response` markers never appear in any argv, on either path, and
    /// the whole payload is forwarded only under `PHUX_AGENT_EMIT_RAW=1`, only
    /// on stdin of a `provider_raw` emit. (Transcript entries also travel on
    /// stdin only; `transcript_entries_ride_provider_raw_on_stdin_by_default`.)
    #[test]
    fn payload_text_never_reaches_a_command_line_unless_raw_is_opted_in() {
        let wrapper = rendered();
        for forbidden in [
            "tool_input",
            "tool_response",
            "transcript_path",
            "$prompt",
            "\"$payload\" |",
        ] {
            assert!(
                !wrapper.contains(forbidden),
                "the wrapper text must not reference `{forbidden}`:\n{wrapper}"
            );
        }

        let h = Harness::new();
        let payload = r#"{"session_id":"sess-1","hook_event_name":"PreToolUse","tool_name":"Bash","prompt":"PROMPT-MARKER","tool_input":{"command":"INPUT-MARKER"}}"#;
        for features in [STREAMING, LEGACY] {
            for arm in [
                "start",
                "working",
                "tool-start",
                "tool-end",
                "blocked",
                "done",
                "clear",
            ] {
                let log = h.hook(
                    arm,
                    payload,
                    features,
                    "sess-1 PreToolUse Bash - 13 - -",
                    false,
                );
                assert!(
                    log.iter().all(|line| !line.contains("MARKER")),
                    "arm `{arm}` leaked payload text: {log:?}"
                );
            }
        }

        let log = h.hook(
            "tool-start",
            payload,
            STREAMING,
            "sess-1 PreToolUse Bash - 13 - -",
            true,
        );
        assert_eq!(
            log,
            [
                "agent hook-payload",
                "status --json",
                "agent emit @42 --type tool_start --data {\"tool_name\":\"Bash\"}",
                "agent emit @42 --type provider_raw --data -",
                &format!("stdin:{payload}"),
            ],
            "raw opt-in forwards the payload once, on stdin, after the typed record"
        );
        // `session_end` is terminal, so on `clear` the raw record goes first.
        let log = h.hook(
            "clear",
            payload,
            STREAMING,
            "sess-1 SessionEnd - - 0 other -",
            true,
        );
        assert_eq!(
            log,
            [
                "agent hook-payload",
                "status --json",
                "agent emit @42 --type provider_raw --data -",
                &format!("stdin:{payload}"),
                "agent emit @42 --type session_end --data {\"reason\":\"other\"}",
                "agent session close @42",
                "agent clear @42",
            ],
            "nothing may be emitted after session_end"
        );
        let log = h.hook(
            "tool-start",
            payload,
            LEGACY,
            "sess-1 PreToolUse Bash - 13 - -",
            true,
        );
        assert!(
            !log.iter().any(|line| line.contains("provider_raw")),
            "raw opt-in has nowhere to go without a session stream: {log:?}"
        );
    }

    /// Transcript entries are on by default (ADR-0156): the `working`,
    /// `tool-end`, and `done` arms feed what `agent hook-transcript` prints to
    /// a `provider_raw` emit on stdin, `done` before its terminal-for-the-turn
    /// `stop`. `PHUX_AGENT_TRANSCRIPT=0` turns them off, an empty helper
    /// answer emits nothing, and the legacy path never asks.
    #[test]
    fn transcript_entries_ride_provider_raw_on_stdin_by_default() {
        let h = Harness::new();
        let entry = r#"{"provider":"claude","schema":"phux.transcript/v1","entry":{"id":"user-1","role":"user","text":"hi","truncated":false,"final":true}}"#;
        let on = [("FAKE_TRANSCRIPT", entry)];
        let fields = "sess-1 UserPromptSubmit - - 2 - -";
        let log = h.hook_env("working", "{}", STREAMING, fields, false, &on);
        assert_eq!(
            log,
            [
                "agent hook-payload",
                "status --json",
                "agent emit @42 --type prompt --data {\"chars\":2}",
                "agent hook-transcript",
                "agent emit @42 --type provider_raw --data -",
                &format!("stdin:{entry}"),
            ]
        );
        let log = h.hook_env("done", "{}", STREAMING, "sess-1 Stop - - 0 - -", false, &on);
        assert_eq!(
            log,
            [
                "agent hook-payload",
                "status --json",
                "agent hook-transcript",
                "agent emit @42 --type provider_raw --data -",
                &format!("stdin:{entry}"),
                "agent emit @42 --type stop",
            ]
        );
        for arm in ["start", "tool-start", "blocked", "clear"] {
            let log = h.hook_env(arm, "{}", STREAMING, fields, false, &on);
            assert!(
                !log.iter().any(|line| line.contains("hook-transcript")),
                "arm `{arm}` has no transcript entry: {log:?}"
            );
        }
        let off = [("FAKE_TRANSCRIPT", entry), ("PHUX_AGENT_TRANSCRIPT", "0")];
        for arm in ["working", "tool-end", "done"] {
            let log = h.hook_env(arm, "{}", STREAMING, fields, false, &off);
            assert!(
                !log.iter()
                    .any(|line| line.contains("transcript") || line.contains("provider_raw")),
                "opted out, arm `{arm}` must not read or emit a transcript: {log:?}"
            );
            let legacy = h.hook_env(arm, "{}", LEGACY, fields, false, &on);
            assert!(
                !legacy.iter().any(|line| line.contains("transcript")),
                "no stream, no transcript: {legacy:?}"
            );
        }
        let silent = h.hook_env("tool-end", "{}", STREAMING, fields, false, &[]);
        assert!(
            !silent.iter().any(|line| line.contains("provider_raw")),
            "an empty helper answer emits nothing: {silent:?}"
        );
    }

    /// The launch path's exit trap runs `clear` at most once, however the
    /// wrapper leaves: INT/TERM/HUP re-enter through EXIT, and the stream's
    /// `session_end` is terminal.
    #[test]
    fn the_exit_trap_is_idempotent() {
        let wrapper = rendered();
        let trap = section(&wrapper, "  ended=false", "  trap 'cleanup' EXIT");
        assert!(
            trap.contains("[ \"$ended\" = false ] || return 0"),
            "{trap}"
        );
        assert!(trap.contains("ended=true"), "{trap}");
        assert!(wrapper.contains("trap 'exit 130' INT"));
        assert!(wrapper.contains("trap 'exit 143' TERM"));
        assert!(wrapper.contains("trap 'exit 129' HUP"));
    }

    /// The wrapper parses under `sh -n` (and, where a `shellcheck` is on
    /// PATH, lints clean at `-s sh`) — it is installed as `/bin/sh` and has
    /// to stay POSIX.
    #[test]
    fn the_wrapper_is_posix_sh() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let wrapper = dir.path().join("claude");
        std::fs::write(&wrapper, rendered()).unwrap();
        let parsed = std::process::Command::new("sh")
            .arg("-n")
            .arg(&wrapper)
            .output()
            .expect("run sh -n");
        assert!(
            parsed.status.success(),
            "sh -n: {}",
            String::from_utf8_lossy(&parsed.stderr)
        );
        if let Ok(lint) = std::process::Command::new("shellcheck")
            .args(["-s", "sh"])
            .arg(&wrapper)
            .output()
        {
            assert!(
                lint.status.success(),
                "shellcheck -s sh:\n{}",
                String::from_utf8_lossy(&lint.stdout)
            );
        }
    }

    /// Install over a stale, unstamped (schema-1) install, then uninstall: the
    /// stale shim is replaced and reported, the hook settings are private, and
    /// uninstall removes exactly its own three files plus the rc block.
    #[test]
    fn install_over_a_stale_shim_upgrades_it_and_uninstall_removes_exactly_its_own_files() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let shim_dir = dir.path().join("shims");
        let rc = dir.path().join("rc");
        let phux = dir.path().join("phux");
        let real = dir.path().join("real-claude");
        std::fs::write(&real, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(installed_shim_schema(&shim_dir.join("claude")), None);

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

        // The hook settings register every `HOOKS` row and stay private.
        let settings = shim_dir.join("claude-hooks.json");
        let mode = std::fs::metadata(&settings).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "hook settings must stay private");
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        let hooks = json["hooks"].as_object().expect("hooks object");
        assert_eq!(hooks.len(), HOOKS.len());
        let shim = sh_quote_path(&shim_dir.join("claude"));
        for (event, matcher, arm) in HOOKS {
            let entry = &hooks[*event][0];
            assert_eq!(entry["matcher"], *matcher, "{event}");
            assert_eq!(
                entry["hooks"][0]["command"],
                format!("{shim} --phux-hook {arm}"),
                "{event}"
            );
        }

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

    /// A rendered wrapper, run for real on a pty (the launch path needs a
    /// tty), must not relaunch Claude after the phux session started and
    /// later died.
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

        /// The `phux` subcommands the wrapper dispatched, one per line.
        fn phux_calls(phux_log: &Path) -> Vec<String> {
            std::fs::read_to_string(phux_log)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

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
            // Running inside a phux pane would send the outer wrapper down the
            // in-session branch, and PHUX_AGENT_PHUX_BIN would override the fake.
            for key in ["PHUX_TERMINAL_ID", "PHUX_AGENT_PHUX_BIN"] {
                command.env_remove(key);
            }
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
            let drain = std::thread::spawn(move || {
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

            // No deadlines: `wait` returns when the wrapper exits and the drain
            // ends at pty EOF, once every spawned process is gone. A wedge
            // hangs here and is bounded by nextest's slow-timeout.
            let status = child.wait().expect("wait for the wrapper to exit");
            drain.join().expect("pty reader thread");
            let text = String::from_utf8_lossy(&output.lock().expect("output lock")).into_owned();
            (status.exit_code(), text)
        }

        let dir = tempfile::tempdir().expect("scratch dir");
        let real = dir.path().join("real-claude");
        let fake_phux = dir.path().join("fake-phux");
        let wrapper = dir.path().join("claude");
        let settings = dir.path().join("claude-hooks.json");
        let log = dir.path().join("real-claude.log");
        // Every `phux` dispatch: distinguishes launched-through-phux from
        // exec'd-directly, which Claude's invocation count cannot.
        let phux_log = dir.path().join("fake-phux.log");

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
            &format!(
                "#!/bin/sh\nset -u\nprintf '%s\\n' \"$1\" >> {phux_log}\nif [ \"${{FAKE_PHUX_LAUNCH_FAIL:-0}}\" = \"1\" ]; then\n  exit 3\nfi\nshift 4\nexec \"$@\"\n",
                phux_log = sh_quote_path(&phux_log),
            ),
        );
        let rendered = render_wrapper(&real, &fake_phux, &wrapper, &settings).unwrap();
        write_script(&wrapper, &rendered);

        // Scenario A: the session runs and Claude exits cleanly.
        std::fs::write(&log, b"").unwrap();
        std::fs::write(&phux_log, b"").unwrap();
        let (status, output) = run_on_pty(&wrapper, &[("FAKE_CLAUDE_EXIT", "0")]);
        assert_eq!(status, 0, "clean session exit; output:\n{output}");
        assert_eq!(
            phux_calls(&phux_log),
            vec!["new".to_owned()],
            "the wrapper must launch Claude THROUGH a phux session, not exec \
             it directly; output:\n{output}"
        );
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count(),
            1,
            "Claude must run exactly once; output:\n{output}"
        );
        assert!(!output.contains("abnormally"), "{output}");
        assert!(!output.contains("launch failed"), "{output}");

        // Scenario B: the session starts, then dies mid-run. The real exit
        // status must propagate and Claude must NOT be invoked again.
        std::fs::write(&log, b"").unwrap();
        std::fs::write(&phux_log, b"").unwrap();
        let (status, output) = run_on_pty(&wrapper, &[("FAKE_CLAUDE_EXIT", "17")]);
        assert_eq!(
            status, 17,
            "mid-session crash status must propagate; output:\n{output}"
        );
        assert_eq!(
            phux_calls(&phux_log),
            vec!["new".to_owned()],
            "the crash must be a crash of the phux-launched session; output:\n{output}"
        );
        assert!(output.contains("phux session ended abnormally"), "{output}");
        assert!(!output.contains("launch failed"), "{output}");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count(),
            1,
            "Claude must not be relaunched after the session already started; output:\n{output}"
        );

        // Scenario C: `phux new` fails before the session starts; falling
        // back to a direct Claude exactly once is correct.
        std::fs::write(&log, b"").unwrap();
        std::fs::write(&phux_log, b"").unwrap();
        let (status, output) = run_on_pty(
            &wrapper,
            &[("FAKE_PHUX_LAUNCH_FAIL", "1"), ("FAKE_CLAUDE_EXIT", "0")],
        );
        assert_eq!(
            status, 0,
            "pre-launch failure falls back to direct Claude; output:\n{output}"
        );
        assert_eq!(
            phux_calls(&phux_log),
            vec!["new".to_owned()],
            "the fallback must follow a launch that was actually ATTEMPTED, \
             and must not retry it; output:\n{output}"
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
