# Slopty shell integration, bash bootstrap.
#
# slopty-ptyd starts an interactive bash with `--rcfile` pointing here, so this file runs instead
# of ~/.bashrc. It first does what bash would have done on its own: for a login shell (the
# daemon moved the `-l` / leading dash into SLOPTY_BASH_LOGIN, because bash ignores --rcfile for
# login shells) /etc/profile and the first of ~/.bash_profile, ~/.bash_login, ~/.profile; for an
# interactive shell ~/.bashrc. --noprofile / --norc travel the same way and are honoured. Then
# it installs the OSC 133 marks:
#
#   133;A / 133;B  around PS1 (inside \[ \], invisible to the width count); A;k=s / B around PS2.
#                  `redraw=last`: after a resize readline redraws the prompt's last row only, so
#                  the terminal clears that row and keeps the ones above
#   133;C          from a DEBUG trap: the first command after a prompt (our own tiny preexec)
#   133;D;<status> from PROMPT_COMMAND, before the next prompt, only when a C is open
#
# If bash-preexec is loaded, its preexec_functions / precmd_functions are used instead of our
# trap. Written by slopty-ptyd on every start; edits here are lost. Opt out with
# SLOPTY_NO_SHELL_INTEGRATION=1. Works on bash 3.2 (macOS /bin/bash) and up.

# A tmux pane is no Slopty tile: its server may have started in another session, whose
# presence file (held while a client looks at that tile) would silence this pane's agents.
[ -n "${TMUX-}" ] && unset CLAUDE_CLIENT_PRESENCE_FILE

if [ -n "${SLOPTY_BASH_LOGIN-}" ]; then
    if [ -z "${SLOPTY_BASH_NOPROFILE-}" ]; then
        [ -r /etc/profile ] && . /etc/profile
        for _slopty_rc in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
            if [ -r "$_slopty_rc" ]; then
                . "$_slopty_rc"
                break
            fi
        done
    fi
elif [ -z "${SLOPTY_BASH_NORC-}" ]; then
    [ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"
fi
unset SLOPTY_BASH_LOGIN SLOPTY_BASH_NOPROFILE SLOPTY_BASH_NORC _slopty_rc

case $- in
    *i*) ;;
    *) return 0 ;;
esac
[ -n "${_slopty_integrated-}" ] && return 0
_slopty_integrated=1
# 1 while a command runs (a C mark is open and needs its D).
_slopty_running=0
# 1 once the prompt is up: the next command the DEBUG trap sees is the user's.
_slopty_armed=0

_slopty_mark() {
    printf '\033]133;%s\007' "$1"
}

# First in PROMPT_COMMAND, so $? is still the command's status.
_slopty_precmd() {
    local _slopty_status=$?
    if [ "$_slopty_running" = 1 ]; then
        _slopty_mark "D;$_slopty_status"
        _slopty_running=0
    fi
    _slopty_armed=0
}

# Last in PROMPT_COMMAND: put SLOPTY_BIN first on the path, report the working directory
# (OSC 7), (re)wrap PS1/PS2 (a theme may have rebuilt them) and arm the trap.
_slopty_arm() {
    # Slopty's `open`, BROWSER and EDITOR first on the path (SLOPTY_BIN): /etc/profile's
    # path_helper and the user's files may have put the system's `open` ahead of it.
    if [ -n "${SLOPTY_BIN-}" ]; then
        case $PATH in
            "$SLOPTY_BIN" | "$SLOPTY_BIN":*) ;;
            *)
                local _slopty_path=":$PATH:"
                _slopty_path=${_slopty_path//":$SLOPTY_BIN:"/:}
                _slopty_path=${_slopty_path#:}
                PATH="$SLOPTY_BIN:${_slopty_path%:}"
                ;;
        esac
    fi
    local _slopty_dir=${PWD//%/%25}
    printf '\033]7;file://%s%s\007' "$HOSTNAME" "${_slopty_dir// /%20}"
    case $PS1 in
        *'133;A'*) ;;
        *) PS1='\[\033]133;A;redraw=last\007\]'"$PS1"'\[\033]133;B\007\]' ;;
    esac
    case $PS2 in
        *'133;A;k=s'*) ;;
        *) PS2='\[\033]133;A;k=s\007\]'"$PS2"'\[\033]133;B\007\]' ;;
    esac
    _slopty_armed=1
}

# The DEBUG trap: fires before every simple command at the top level, including the ones in
# PROMPT_COMMAND; only the first one after a prompt is the user's command.
_slopty_preexec() {
    [ "$_slopty_armed" = 1 ] || return 0
    case $BASH_COMMAND in
        _slopty_*) return 0 ;;
    esac
    _slopty_armed=0
    _slopty_running=1
    _slopty_mark C
}

# bash-preexec's hooks: it owns the DEBUG trap and calls these itself.
_slopty_bp_preexec() {
    _slopty_running=1
    _slopty_mark C
}
_slopty_bp_precmd() {
    local _slopty_status=$?
    if [ "$_slopty_running" = 1 ]; then
        _slopty_mark "D;$_slopty_status"
        _slopty_running=0
    fi
    _slopty_arm
}

if [ -n "${__bp_imported-}" ] || [ -n "${bash_preexec_imported-}" ]; then
    preexec_functions+=(_slopty_bp_preexec)
    precmd_functions+=(_slopty_bp_precmd)
elif [ -z "$(trap -p DEBUG)" ]; then
    trap '_slopty_preexec' DEBUG
    case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
        'declare -a'*) PROMPT_COMMAND=(_slopty_precmd "${PROMPT_COMMAND[@]}" _slopty_arm) ;;
        *) PROMPT_COMMAND="_slopty_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}; _slopty_arm" ;;
    esac
else
    # Someone else's DEBUG trap: prompt marks only, no command status.
    case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
        'declare -a'*) PROMPT_COMMAND=("${PROMPT_COMMAND[@]}" _slopty_arm) ;;
        *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND; }_slopty_arm" ;;
    esac
fi

# `sudo` keeps the terminfo (ghostty's `sudo` feature): TERM names our entry, and TERMINFO
# says where it is, so `sudo vim` without it would find no terminal at all. sudoedit (`-e`,
# `--edit`) takes no --preserve-env and is left alone. An alias of sudo defined after this
# wins over it; one defined before is wrapped.
if [ -n "${TERMINFO-}" ]; then
    sudo() {
        local arg edit=0
        for arg in "$@"; do
            case $arg in
                -e|--edit) edit=1; break ;;
                -*|*=*) ;;
                *) break ;;
            esac
        done
        if [ "$edit" = 1 ]; then
            command sudo "$@"
        else
            command sudo --preserve-env=TERMINFO "$@"
        fi
    }
fi

# `claude` starts the user's own Claude Code wired as an agent Slopty starts: its hooks reach
# the worker (status, permission prompts), it has Slopty's tools when the worker has a server,
# and it loads Slopty's mod. `slopty hook wire` says how, as NUL-ended words: the variables to
# set, an empty word, then the arguments, the user's own among them. Without that answer (an
# older CLI, an error) the call goes through as typed. A `claude` of the user's own (an alias
# or a function) is left alone, and SLOPTY_NO_CLAUDE_MOD=1 passes every call through untouched.
# bash 3.2 (macOS's) drops NULs from a command substitution, so the words are read one by one.
if [ -n "${SLOPTY_CLI-}" ] && [ -x "$SLOPTY_CLI" ] && ! declare -F claude >/dev/null && ! alias claude >/dev/null 2>&1; then
    function claude {
        if [ -n "${SLOPTY_NO_CLAUDE_MOD-}" ] && [ "$SLOPTY_NO_CLAUDE_MOD" != 0 ]; then
            command claude "$@"
            return
        fi
        local _slopty_word _slopty_args=0
        local -a _slopty_env=() _slopty_argv=()
        while IFS= read -r -d '' _slopty_word; do
            if [ "$_slopty_args" = 1 ]; then
                _slopty_argv+=("$_slopty_word")
            elif [ -z "$_slopty_word" ]; then
                _slopty_args=1
            else
                _slopty_env+=("$_slopty_word")
            fi
        done < <("$SLOPTY_CLI" hook wire -- "$@" 2>/dev/null)
        if [ "$_slopty_args" != 1 ]; then
            command claude "$@"
            return
        fi
        [ "${#_slopty_env[@]}" -gt 0 ] && local -x "${_slopty_env[@]}"
        command claude ${_slopty_argv[@]+"${_slopty_argv[@]}"}
    }
fi

# `ssh` keeps a terminal the far side knows (ghostty's `ssh-env` and `ssh-terminfo`): `slopty
# ssh` installs our terminfo entry on the host once, else the session gets xterm-256color, and
# SLOPTY_NO_SSH_TERMINFO=1 leaves every host untouched. An `ssh` of the user's own (an alias or
# a function) is left alone, and `command ssh` is always the plain one.
if [ -n "${SLOPTY_CLI-}" ] && [ -x "$SLOPTY_CLI" ] && ! declare -F ssh >/dev/null && ! alias ssh >/dev/null 2>&1; then
    function ssh {
        "$SLOPTY_CLI" ssh -- "$@"
    }
fi
