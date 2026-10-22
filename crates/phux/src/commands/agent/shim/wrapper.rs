//! Rendering the generated `/bin/sh` wrapper.

use std::path::Path;

use crate::commands::sh_quote;

use super::{SCHEMA_MARKER, SHIM_SCHEMA};

/// Render the `/bin/sh` wrapper installed as `claude`.
///
/// Every `--phux-hook` arm reads Claude's stdin payload through
/// `phux agent hook-payload`, then: `start` writes identity once (never a
/// `--state`, which would outrank the server's derivation, docs/program-sequences.md);
/// the lifecycle arms feed the pane's `AgentSession` stream with
/// `agent emit`; `blocked` also calls `phux ask` and `clear` deletes the
/// record. A session is opened at most once per process and never for a
/// `compact`-sourced `SessionStart`.
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
pub(super) fn render_wrapper(
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
  entry=
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
      if [ "$hook_event" = PostToolUseFailure ] && [ "$hook_tool" != - ]; then
        emit tool_end "{{\"tool_name\":\"$hook_tool\",\"ok\":false}}"
      else
        emit_tool tool_end
      fi
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

set_state() {{
  [ -n "${{PHUX_TERMINAL_ID:-}}" ] || return 0
  target="@$PHUX_TERMINAL_ID"
  # Identity is written once, at start.
  case "$1" in
    start) run_phux agent set "$target" --name claude --kind claude ;;
  esac
  stream_state "$1"
  # The attention ladder and the record cleanup.
  case "$1" in
    blocked) run_phux ask "$target" "Claude needs attention" ;;
    clear) run_phux agent clear "$target" ;;
  esac
}}

if [ "${{1:-}}" = "--phux-hook" ]; then
  [ "$#" -eq 2 ] || exit 2
  # The payload copy and a transcript entry are removed however the hook
  # leaves, including Claude killing it at its timeout.
  entry=
  trap 'rm -f ${{payload:+"$payload"}} ${{entry:+"$entry"}}' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  read_payload
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
        real = sh_quote(&real.to_string_lossy()),
        phux = sh_quote(&phux.to_string_lossy()),
        shim = sh_quote(&shim.to_string_lossy()),
        settings = sh_quote(&settings.to_string_lossy()),
    ))
}
