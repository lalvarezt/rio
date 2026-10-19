# Reports the working directory to rio (OSC 7) on every prompt and
# directory change, which powers the title path variables and opening
# new tabs in the current directory.

# Rio prepended its script directory to XDG_DATA_DIRS so this file
# loads; restore the original value (empty means it was unset) so the
# prepend never leaks to child processes.
if set -q RIO_FISH_XDG_DATA_DIRS
    if test -n "$RIO_FISH_XDG_DATA_DIRS"
        set -gx XDG_DATA_DIRS "$RIO_FISH_XDG_DATA_DIRS"
    else
        set -e XDG_DATA_DIRS
    end
    set -e RIO_FISH_XDG_DATA_DIRS
end

status is-interactive; or exit

function __rio_report_pwd --on-variable PWD --on-event fish_prompt \
        --description 'Report the working directory to rio (OSC 7)'
    printf '\e]7;kitty-shell-cwd://%s%s\a' $hostname $PWD
end
__rio_report_pwd

# Shell reports identify remote apps that local process inspection cannot see.
function __rio_report_app --argument-names app
    set -l transport MA==
    if set -q SSH_CONNECTION; or set -q SSH_CLIENT; or set -q SSH_TTY
        set transport MQ==
    end
    set -l encoded (printf '%s' "$app" | base64 | string join '')
    printf '\e]1337;SetUserVar=RIO_SSH=%s\a\e]1337;SetUserVar=RIO_APP=%s\a' "$transport" "$encoded"
end

function __rio_preexec --on-event fish_preexec
    set -l words (string match -ra '\\S+' -- "$argv[1]")
    while test (count $words) -gt 1
        if contains -- $words[1] command exec env sudo doas; or string match -q '*=*' -- $words[1]
            set -e words[1]
        else
            break
        end
    end
    if test (count $words) -gt 0
        __rio_report_app (string trim -c '\'"' -- $words[1])
    end
end

function __rio_prompt_app --on-event fish_prompt
    __rio_report_app fish
end
__rio_prompt_app
