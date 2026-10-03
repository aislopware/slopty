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
#   OSC 7          the working directory, percent-encoded, before each prompt
#
# In a tmux pane every mark goes through tmux's passthrough (DCS `tmux;`, each ESC doubled),
# since tmux keeps OSC 133 and OSC 7 to itself, and the pane allows it (`allow-passthrough`,
# off by default since tmux 3.3; `on` passes only while the pane is visible, which is when its
# marks match the screen Slopty draws).
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

# How an OSC starts and ends here, and the end as PS1 spells it (its backslash doubled). A
# shell ptyd started is told TERM_PROGRAM=slopty, and tmux sets its own in a pane, so a TMUX
# inherited from a daemon that ran under tmux does not count.
if [ -n "${TMUX-}" ] && [ "${TERM_PROGRAM-}" != slopty ]; then
    _slopty_osc=$'\ePtmux;\e\e]'
    _slopty_st=$'\a\e\\'
    _slopty_ps_st=$'\a\e\\\\'
    command tmux set-option -p allow-passthrough on >/dev/null 2>&1
else
    _slopty_osc=$'\e]'
    _slopty_st=$'\a'
    _slopty_ps_st=$'\a'
fi

_slopty_mark() {
    printf '%s133;%s%s' "$_slopty_osc" "$1" "$_slopty_st"
}

# The working directory as an OSC 7 URL's path: every byte but the unreserved ones and `/`
# percent-encoded (RFC 3986), as a URL parser reads it back. Worked out again only when the
# directory changed, and byte by byte only when something in it needs encoding.
_slopty_cwd_url() {
    [ "$PWD" = "${_slopty_cwd_of-}" ] && return 0
    local LC_ALL=C _slopty_i=0 _slopty_c _slopty_h _slopty_out=
    _slopty_cwd_of=$PWD
    case $PWD in
        *[!A-Za-z0-9/._~-]*) ;;
        *)
            _slopty_cwd_path=$PWD
            return 0
            ;;
    esac
    while [ "$_slopty_i" -lt "${#PWD}" ]; do
        _slopty_c=${PWD:_slopty_i:1}
        case $_slopty_c in
            [A-Za-z0-9/._~-]) _slopty_out=$_slopty_out$_slopty_c ;;
            *)
                # bash 3.2 gives a byte past 0x7f as a negative number: its last two digits.
                printf -v _slopty_h '%02X' "'$_slopty_c"
                _slopty_out=$_slopty_out%${_slopty_h: -2}
                ;;
        esac
        _slopty_i=$((_slopty_i + 1))
    done
    _slopty_cwd_path=$_slopty_out
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
    _slopty_cwd_url
    printf '%s7;file://%s%s%s' "$_slopty_osc" "$HOSTNAME" "$_slopty_cwd_path" "$_slopty_st"
    local _slopty_b='\['"${_slopty_osc}133;B${_slopty_ps_st}"'\]'
    case $PS1 in
        *'133;A'*) ;;
        *) PS1='\['"${_slopty_osc}133;A;redraw=last${_slopty_ps_st}"'\]'"$PS1$_slopty_b" ;;
    esac
    case $PS2 in
        *'133;A;k=s'*) ;;
        *) PS2='\['"${_slopty_osc}133;A;k=s${_slopty_ps_st}"'\]'"$PS2$_slopty_b" ;;
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

# `codex` runs without what names this terminal (SLOPTY_SESSION, its token, SLOPTY_PROJECT and
# SLOPTY_TASK). Codex's app-server daemon, which the first `codex` starts, keeps its starter's
# environment for every thread's commands, so a daemon started here would have every Codex
# thread, whoever started it, speak as this terminal. A `codex` of the user's own (an alias
# or a function) is left alone.
if [ -n "${SLOPTY_SESSION-}" ] && ! declare -F codex >/dev/null && ! alias codex >/dev/null 2>&1; then
    function codex {
        command env -u SLOPTY_SESSION -u SLOPTY_SESSION_TOKEN -u SLOPTY_PROJECT -u SLOPTY_TASK \
            codex "$@"
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
