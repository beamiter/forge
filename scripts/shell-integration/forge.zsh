# forge shell integration for zsh.
# Source from ~/.zshrc, for example:
#   [[ $TERM_PROGRAM == forge ]] && source /path/to/forge.zsh

[[ -n ${__FORGE_ZSH_LOADED:-} ]] && return 0
__FORGE_ZSH_LOADED=1

__forge_osc() { printf '\033]%s\007' "$1"; }
__forge_prompt_start() { __forge_osc "133;A"; }
__forge_prompt_end() { __forge_osc "133;B"; }
typeset -g __forge_command_token=
typeset -g __forge_token_fd=${FORGE_SHELL_INTEGRATION_FD:-}
unset FORGE_SHELL_INTEGRATION_FD FORGE_SHELL_INTEGRATION_TOKEN
if [[ $__forge_token_fd == <-> ]]; then
    IFS= read -r -u "$__forge_token_fd" __forge_command_token || __forge_command_token=
    # The descriptor number passed the zsh numeric glob above.
    eval "exec ${__forge_token_fd}<&-"
fi
unset __forge_token_fd
if (( ${#__forge_command_token} == 32 )) \
    && [[ $__forge_command_token != *[^[:xdigit:]]* ]]; then
    __forge_agent_ready() { __forge_osc "7771;${__forge_command_token}"; }
else
    __forge_command_token=forge-zsh-$$
    __forge_agent_ready() { :; }
fi
typeset -gi __forge_command_seq=0
typeset -g __forge_command_id=
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
    local host=${HOST:-${HOSTNAME:-localhost}}
    local out= i ch
    for ((i = 1; i <= ${#PWD}; i++)); do
        ch=${PWD[i]}
        case $ch in
            [A-Za-z0-9._~/-]) out+=$ch ;;
            *) printf -v out '%s%%%02X' "$out" "'$ch" ;;
        esac
    done
    __forge_osc "7;file://${host}${out}"
}

__forge_in_command=0
__forge_preexec() {
    if (( __forge_in_command == 0 )); then
        __forge_in_command=1
        __forge_command_start
    fi
}
__forge_precmd() {
    local ec=$?
    if (( __forge_in_command == 1 )); then
        __forge_command_end "$ec"
        __forge_in_command=0
    fi
    __forge_report_cwd
    __forge_prompt_start
    __forge_agent_ready
}

if [[ -z ${__FORGE_PS1_HOOKED:-} ]]; then
    PS1="${PS1}%{$(__forge_prompt_end)%}"
    __FORGE_PS1_HOOKED=1
fi

autoload -Uz add-zsh-hook
add-zsh-hook preexec __forge_preexec
add-zsh-hook precmd __forge_precmd
export TERM_PROGRAM=forge
