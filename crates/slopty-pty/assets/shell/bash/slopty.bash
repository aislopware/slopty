# Slopty shell integration, bash bootstrap.
#
# slopty-ptyd starts an interactive bash with `--rcfile` pointing here, so this file runs instead
# of ~/.bashrc. It first does what bash would have done on its own: for a login shell (the
# daemon moved the `-l` / leading dash into SLOPTY_BASH_LOGIN, because bash ignores --rcfile for
# login shells) /etc/profile and the first of ~/.bash_profile, ~/.bash_login, ~/.profile; for an
# interactive shell ~/.bashrc. --noprofile / --norc travel the same way and are honoured. Then
# it installs the OSC 133 marks:
#
#   133;A / 133;B  around PS1 (inside \[ \], invisible to the width count); A;k=s / B around PS2
#   133;C          from a DEBUG trap: the first command after a prompt (our own tiny preexec)
#   133;D;<status> from PROMPT_COMMAND, before the next prompt, only when a C is open
#
# If bash-preexec is loaded, its preexec_functions / precmd_functions are used instead of our
# trap. Written by slopty-ptyd on every start; edits here are lost. Opt out with
# SLOPTY_NO_SHELL_INTEGRATION=1. Works on bash 3.2 (macOS /bin/bash) and up.

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

# Last in PROMPT_COMMAND: (re)wrap PS1/PS2 (a theme may have rebuilt them) and arm the trap.
_slopty_arm() {
    case $PS1 in
        *'133;A'*) ;;
        *) PS1='\[\033]133;A\007\]'"$PS1"'\[\033]133;B\007\]' ;;
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
