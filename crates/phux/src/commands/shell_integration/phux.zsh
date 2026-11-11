# phux shell integration for zsh: prompt and command marks (OSC 133).
# Load it from ~/.zshrc:  eval "$(phux config shell-integration zsh)"
if [[ -o interactive && -z $_phux_marks ]]; then
  _phux_marks=1
  _phux_precmd() {
    local code=$?
    [[ -n $_phux_ran ]] && print -n "\e]133;D;$code\a"
    _phux_ran=
    print -n '\e]133;A\a'
  }
  _phux_preexec() {
    _phux_ran=1
    print -n '\e]133;C\a'
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook precmd _phux_precmd
  add-zsh-hook preexec _phux_preexec
fi
