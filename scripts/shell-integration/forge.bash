# forge shell integration for bash.
# Source from ~/.bashrc, for example:
#   [[ $TERM_PROGRAM == forge ]] && source /path/to/forge.bash

[[ -n ${__FORGE_BASH_LOADED:-} ]] && return 0
__FORGE_BASH_LOADED=1
__forge_integration_source=${BASH_SOURCE[0]}

# Integration records belong to the controlling terminal, including when Bash
# applies a compound command's redirections before invoking DEBUG. Open only
# for this write: no persistent descriptor can leak into child commands. If
# there is no controlling terminal, omit the record rather than pollute stdout.
__forge_osc_shell_pid=${BASHPID:-$$}
__forge_osc() {
    [[ ${BASHPID:-$$} == "$__forge_osc_shell_pid" && ${BASH_SUBSHELL:-0} == 0 ]] || return 0
    builtin printf '\033]%s\007' "$1" 2>/dev/null >/dev/tty || :
}
__forge_prompt_start() { __forge_osc "133;A"; }
__forge_command_token=
__forge_token_fd=${FORGE_SHELL_INTEGRATION_FD:-}
unset FORGE_SHELL_INTEGRATION_FD FORGE_SHELL_INTEGRATION_TOKEN
if [[ $__forge_token_fd =~ ^[0-9]+$ ]]; then
    IFS= read -r -u "$__forge_token_fd" __forge_command_token || __forge_command_token=
    # The descriptor number came from a digits-only check above. Close it
    # before any user command can inherit the one-shot token channel.
    eval "exec ${__forge_token_fd}<&-"
fi
unset __forge_token_fd
if [[ $__forge_command_token =~ ^[[:xdigit:]]{32}$ ]]; then
    __forge_agent_ready() { __forge_osc "7771;${__forge_command_token}"; }
else
    __forge_command_token=forge-bash-${BASHPID:-$$}
    __forge_agent_ready() { :; }
fi
__forge_command_seq=0
__forge_command_id=
__forge_command_start() {
    ((__forge_command_seq += 1))
    __forge_command_id="${__forge_command_token}-${__forge_command_seq}"
    __forge_osc "133;C;id=${__forge_command_id}"
}
__forge_command_end() {
    __forge_osc "133;D;$1;id=${__forge_command_id}"
    __forge_command_id=
}

__forge_report_cwd() {
    local host=${HOSTNAME:-localhost}
    local out= i ch
    local LC_ALL=C
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
    __forge_agent_ready
    if [[ -z ${__FORGE_PS1_HOOKED:-} ]]; then
        # This marker is prompt data, rendered by Bash after the visible PS1.
        PS1+=$'\\[\033]133;B\007\\]'
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
