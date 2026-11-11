# phux shell integration for bash: prompt and command marks (OSC 133).
# Load it from ~/.bashrc:  eval "$(phux config shell-integration bash)"
# Command-start marks need bash 4.4 or newer (PS0); older bash marks prompts only.
if [[ $- == *i* && -z $_phux_marks ]]; then
  _phux_marks=1
  _phux_prompt() {
    local code=$?
    [[ -n $_phux_seen ]] && printf '\033]133;D;%s\a' "$code"
    _phux_seen=1
    printf '\033]133;A\a'
  }
  PROMPT_COMMAND="_phux_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
  PS0=$'\033]133;C\a'"${PS0-}"
fi
