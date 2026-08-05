# forge shell integration for bash.
# Source from ~/.bashrc, for example:
#   [[ $TERM_PROGRAM == forge ]] && source /path/to/forge.bash

[[ -n ${__FORGE_BASH_LOADED:-} ]] && return 0
__FORGE_BASH_LOADED=1
__forge_integration_source=${BASH_SOURCE[0]}

__forge_osc() { printf '\033]%s\007' "$1"; }
__forge_prompt_start() { __forge_osc "133;A"; }
__forge_prompt_end() { __forge_osc "133;B"; }
__forge_command_start() { __forge_osc "133;C"; }
__forge_command_end() { __forge_osc "133;D;$1"; }

__forge_report_cwd() {
    local host=${HOSTNAME:-localhost}
    local out= i ch
    LC_ALL=C
    for ((i = 0; i < ${#PWD}; i++)); do
        ch=${PWD:i:1}
        case $ch in
            [A-Za-z0-9._~/-]) out+=$ch ;;
            *) printf -v out '%s%%%02X' "$out" "'$ch" ;;
        esac
    done
    __forge_osc "7;file://${host}${out}"
}

__forge_in_command=0
__forge_in_prompt_command=0

__forge_preexec() {
    [[ -n ${COMP_LINE:-} ]] && return
    [[ ${BASH_SOURCE[1]:-} == "$__forge_integration_source" ]] && return

    # DEBUG fires before PROMPT_COMMAND and, with functrace enabled, inside its
    # functions too. Mark the complete prompt phase here so neither our hook nor
    # a user's saved PROMPT_COMMAND is mistaken for a submitted shell command.
    if [[ ${BASH_COMMAND} == "__forge_prompt_command" ]]; then
        __forge_in_prompt_command=1
        return
    fi
    (( __forge_in_prompt_command == 1 )) && return

    if (( __forge_in_command == 0 )); then
        __forge_in_command=1
        __forge_command_start
    fi
}

__forge_precmd() {
    local ec=$1
    if (( __forge_in_command == 1 )); then
        __forge_command_end "$ec"
        __forge_in_command=0
    fi
    __forge_report_cwd
    __forge_prompt_start
    if [[ -z ${__FORGE_PS1_HOOKED:-} ]]; then
        PS1="${PS1}\[$(__forge_prompt_end)\]"
        __FORGE_PS1_HOOKED=1
    fi
}

# Preserve every existing prompt hook, including Bash 5's array form, while
# making our dispatcher the sole PROMPT_COMMAND visible to the DEBUG trap.
__forge_saved_prompt_commands=("${PROMPT_COMMAND[@]:-}")
__forge_prompt_command() {
    local ec=$?
    local command
    __forge_in_prompt_command=1
    __forge_precmd "$ec"
    for command in "${__forge_saved_prompt_commands[@]}"; do
        [[ -n $command ]] && builtin eval -- "$command"
    done
    __forge_in_prompt_command=0
}

unset PROMPT_COMMAND
PROMPT_COMMAND=__forge_prompt_command
export TERM_PROGRAM=forge
trap '__forge_preexec' DEBUG
