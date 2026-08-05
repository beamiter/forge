# forge shell integration for zsh.
# Source from ~/.zshrc, for example:
#   [[ $TERM_PROGRAM == forge ]] && source /path/to/forge.zsh

[[ -n ${__FORGE_ZSH_LOADED:-} ]] && return 0
__FORGE_ZSH_LOADED=1

__forge_osc() { printf '\033]%s\007' "$1"; }
__forge_prompt_start() { __forge_osc "133;A"; }
__forge_prompt_end() { __forge_osc "133;B"; }
__forge_command_start() { __forge_osc "133;C"; }
__forge_command_end() { __forge_osc "133;D;$1"; }

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
}

if [[ -z ${__FORGE_PS1_HOOKED:-} ]]; then
    PS1="${PS1}%{$(__forge_prompt_end)%}"
    __FORGE_PS1_HOOKED=1
fi

autoload -Uz add-zsh-hook
add-zsh-hook preexec __forge_preexec
add-zsh-hook precmd __forge_precmd
export TERM_PROGRAM=forge
