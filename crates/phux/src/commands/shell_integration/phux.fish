# phux shell integration for fish: prompt and command marks (OSC 133).
# Load it from ~/.config/fish/config.fish:  phux config shell-integration fish | source
# fish 4.0 and newer emit these marks themselves; this is for older fish.
if status is-interactive; and not set -q _phux_marks
    set -g _phux_marks 1
    function _phux_prompt --on-event fish_prompt
        printf '\e]133;A\a'
    end
    function _phux_preexec --on-event fish_preexec
        printf '\e]133;C\a'
    end
    function _phux_postexec --on-event fish_postexec
        printf '\e]133;D;%s\a' $status
    end
end
