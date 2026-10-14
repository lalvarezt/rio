# Generic app/SSH reporting for Bash; source from an interactive shell.
[[ $- == *i* ]] || return
[[ -z ${_rio_bash_context_loaded-} ]] || return
_rio_bash_context_loaded=1

_rio_report_app() {
    local app="${1##*/}" remote=0 encoded transport
    [[ -n ${SSH_CONNECTION-}${SSH_CLIENT-}${SSH_TTY-} ]] && remote=1
    encoded=$(printf '%s' "$app" | base64 | tr -d '\n')
    transport=$(printf '%s' "$remote" | base64 | tr -d '\n')
    printf '\e]1337;SetUserVar=RIO_SSH=%s\a\e]1337;SetUserVar=RIO_APP=%s\a' "$transport" "$encoded"
}

_rio_preexec() {
    local previous_status=$? line="${1-}" app
    local -a words
    [[ ${_rio_at_prompt-0} == 1 ]] || return "$previous_status"
    case "$line" in _*|PROMPT_COMMAND*|trap\ *) return "$previous_status" ;; esac
    read -r -a words <<< "$line"
    while (( ${#words[@]} > 1 )); do
        case "${words[0]}" in
            command|builtin|exec|env|sudo|doas|*=*) words=("${words[@]:1}") ;;
            *) break ;;
        esac
    done
    app=${words[0]-bash}
    app=${app#\"}; app=${app%\"}; app=${app#\'}; app=${app%\'}
    _rio_report_app "$app"
    return "$previous_status"
}

_rio_prompt_app() {
    local previous_status=$?
    _rio_report_app bash
    _rio_at_prompt=1
    return "$previous_status"
}

if declare -p preexec_functions precmd_functions >/dev/null 2>&1; then
    preexec_functions+=(_rio_preexec)
    precmd_functions+=(_rio_prompt_app)
else
    # Keep an existing DEBUG handler and its exit status.
    _rio_original_debug=$(trap -p DEBUG)
    if [[ -n $_rio_original_debug ]]; then
        _rio_original_debug=${_rio_original_debug#trap -- }
        _rio_original_debug=${_rio_original_debug% DEBUG}
        eval "_rio_original_debug=$_rio_original_debug"
        trap '_rio_preexec "$BASH_COMMAND"; eval "$_rio_original_debug"' DEBUG
    else
        trap '_rio_preexec "$BASH_COMMAND"; :' DEBUG
    fi
    if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a'* ]]; then
        PROMPT_COMMAND+=(_rio_prompt_app)
    else
        PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND; }_rio_prompt_app"
    fi
fi
_rio_prompt_app
