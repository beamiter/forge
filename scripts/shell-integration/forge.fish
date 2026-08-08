# forge shell integration for fish.
# Source from ~/.config/fish/config.fish, for example:
#   if test "$TERM_PROGRAM" = forge; source /path/to/forge.fish; end

if set -q __forge_fish_loaded
    return 0
end
set -g __forge_fish_loaded 1

function __forge_osc
    printf '\033]%s\007' $argv[1]
end

function __forge_report_cwd --on-variable PWD
    set -l host (hostname 2>/dev/null; or echo localhost)
    set -l enc (string escape --style=url -- $PWD)
    __forge_osc "7;file://$host$enc"
end

function __forge_prompt_start  ; __forge_osc "133;A" ; end
function __forge_prompt_end    ; __forge_osc "133;B" ; end
set -e FORGE_SHELL_INTEGRATION_FD FORGE_SHELL_INTEGRATION_TOKEN
set -g __forge_command_token "forge-fish-$fish_pid"
set -g __forge_command_seq 0
set -g __forge_command_id ""
function __forge_command_start
    set -g __forge_command_seq (math $__forge_command_seq + 1)
    set -g __forge_command_id "$__forge_command_token-$__forge_command_seq"
    __forge_osc "133;C;id=$__forge_command_id"
end
function __forge_command_end
    __forge_osc "133;D;$argv[1];id=$__forge_command_id"
    set -g __forge_command_id ""
end

function __forge_preexec --on-event fish_preexec
    __forge_command_start
end

function __forge_postexec --on-event fish_postexec
    __forge_command_end $status
end

if not functions -q __forge_orig_prompt
    functions -c fish_prompt __forge_orig_prompt
    function fish_prompt
        __forge_prompt_start
        __forge_orig_prompt
        __forge_prompt_end
    end
end

__forge_report_cwd
set -gx TERM_PROGRAM forge
