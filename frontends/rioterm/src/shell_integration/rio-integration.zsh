# Reports the working directory to rio (OSC 7) on every prompt and
# directory change, which powers the title path variables and opening
# new tabs in the current directory. Loading twice is a no-op.
if [[ -n "${_rio_integration+x}" ]]; then
    return 0
fi
builtin typeset -g _rio_integration=1

_rio_report_pwd() {
    builtin printf '\e]7;kitty-shell-cwd://%s%s\a' "${HOST-}" "$PWD"
}

builtin autoload -Uz add-zsh-hook
add-zsh-hook chpwd _rio_report_pwd
add-zsh-hook precmd _rio_report_pwd
_rio_report_pwd

_rio_report_app() {
    local remote=0 app="${1:t}"
    [[ -n "${SSH_CONNECTION-}${SSH_CLIENT-}${SSH_TTY-}" ]] && remote=1
    local encoded="$(builtin printf '%s' "$app" | base64 | tr -d '\n')"
    local transport="$(builtin printf '%s' "$remote" | base64 | tr -d '\n')"
    builtin printf '\e]1337;SetUserVar=RIO_SSH=%s\a\e]1337;SetUserVar=RIO_APP=%s\a' "$transport" "$encoded"
}
_rio_preexec() {
    local -a words
    words=( ${(z)1} )
    while (( ${#words} > 1 )); do
        case "${words[1]}" in
            command|exec|env|sudo|doas|*=*) shift words ;;
            *) break ;;
        esac
    done
    _rio_report_app "${(Q)words[1]}"
}
_rio_prompt_app() { _rio_report_app zsh }
add-zsh-hook preexec _rio_preexec
add-zsh-hook precmd _rio_prompt_app
_rio_prompt_app
