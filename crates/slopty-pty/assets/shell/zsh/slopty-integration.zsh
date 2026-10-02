# Slopty shell integration for zsh: OSC 133 prompt marks, so the client can draw command
# blocks, jump between prompts, copy a command's output and tint a failed command.
#
#   133;A  prompt starts (put in PS1, so a theme that rebuilds the prompt keeps it); `redraw=1`
#          says zle redraws all of it after a resize, so the terminal clears it first
#   133;B  input starts (end of PS1)
#   133;C  output starts (preexec)
#   133;D;<status>  command ended (precmd, before the next prompt)
#   OSC 7  the working directory (precmd), so the client can name where a shell is
#
# Before each prompt SLOPTY_BIN goes back to the front of the path: its `open` hands web pages to
# the client, and the system's `/usr/bin/open` must not be found first.
#
# Sourced by the bootstrap .zshenv for interactive shells; safe to source twice.

[[ -n "${_slopty_integrated+set}" ]] && return 0
'builtin' 'typeset' -g _slopty_integrated=1
# A tmux pane is no Slopty tile: its server may have started in another session, whose
# presence file (held while a client looks at that tile) would silence this pane's agents.
[[ -n "${TMUX-}" ]] && 'builtin' 'unset' CLAUDE_CLIENT_PRESENCE_FILE
# 1 while a command runs (a C mark is open and needs its D).
'builtin' 'typeset' -gi _slopty_running=0

_slopty_precmd() {
    'builtin' 'local' -i _slopty_last=$?
    if (( _slopty_running )); then
        'builtin' 'print' -n -- $'\e]133;D;'"$_slopty_last"$'\a'
        _slopty_running=0
    fi
    # Slopty's `open`, BROWSER and EDITOR first on the path (SLOPTY_BIN): /etc/zprofile's
    # path_helper and the user's files may have put the system's `open` ahead of it.
    if [[ -n "${SLOPTY_BIN-}" && "${path[1]-}" != "$SLOPTY_BIN" ]]; then
        path=("$SLOPTY_BIN" "${(@)path:#$SLOPTY_BIN}")
    fi
    'builtin' 'local' _slopty_dir=${PWD//\%/%25}
    'builtin' 'print' -n -- $'\e]7;file://'"${HOST}${_slopty_dir// /%20}"$'\a'
    # Marks inside PS1/PS2 so zle redraws keep them; %{ %} hides them from width counting.
    if [[ "$PS1" != *$'\e]133;A'* ]]; then
        PS1=$'%{\e]133;A;redraw=1\a%}'"$PS1"$'%{\e]133;B\a%}'
    fi
    if [[ -n "$PS2" && "$PS2" != *$'\e]133;A;k=s'* ]]; then
        PS2=$'%{\e]133;A;k=s\a%}'"$PS2"$'%{\e]133;B\a%}'
    fi
    # Run after every other precmd hook so a prompt theme cannot rebuild PS1 behind us.
    if [[ "${precmd_functions[-1]}" != _slopty_precmd ]]; then
        precmd_functions=("${(@)precmd_functions:#_slopty_precmd}" _slopty_precmd)
    fi
}

_slopty_preexec() {
    # The cursor is the program's shape again before the command runs.
    'builtin' 'print' -n -- $'\e[0 q\e]133;C\a'
    _slopty_running=1
}

'builtin' 'typeset' -ga precmd_functions preexec_functions
precmd_functions+=(_slopty_precmd)
preexec_functions+=(_slopty_preexec)

# The cursor says which keymap zle is in (ghostty's `cursor` feature): a blinking bar to
# insert, a blinking block in vi command or visual mode. Hooked through
# add-zle-hook-widget when the widget is already one of its hooks; otherwise the widget in
# place (a plugin's, or none) is kept and called after ours, as ghostty does, since
# add-zle-hook-widget over a hand-set widget breaks that widget.
_slopty_zle_cursor() {
    case ${KEYMAP-} in
        vicmd|visual) 'builtin' 'print' -n -- $'\e[1 q' ;;
        *)            'builtin' 'print' -n -- $'\e[5 q' ;;
    esac
}
# A prompt whose PS1 lost the marks (a theme that rebuilt it after our precmd, before the
# hook order settled) still gets them when zle starts reading: `P` marks the row in place (no
# fresh line, the prompt is already drawn) and `B` the input, so blocks and click-to-move hold.
_slopty_zle_marks() {
    if [[ "$PS1" != *$'\e]133;A'* ]]; then
        'builtin' 'print' -n -- $'\e]133;P;k=i\a\e]133;B\a'
    fi
}
() {
    'builtin' 'local' hook widget func orig flag
    for hook in line-init line-finish keymap-select; do
        widget=zle-$hook
        func=_slopty_zle_${hook/-/_}
        functions[$func]='_slopty_zle_cursor'
        [[ $hook == line-init ]] && functions[$func]='_slopty_zle_marks; _slopty_zle_cursor'
        if [[ $widgets[$widget] == user:azhw:* && $+functions[add-zle-hook-widget] -eq 1 ]]; then
            add-zle-hook-widget $hook $func
        else
            if (( $+widgets[$widget] )); then
                orig=._slopty_orig_$widget
                'builtin' 'zle' -A $widget $orig
                flag=
                [[ $widgets[$widget] == user:* ]] || flag=w
                functions[$func]+="
                    'builtin' 'zle' $orig -N$flag -- \"\$@\""
            fi
            'builtin' 'zle' -N $widget $func
        fi
    done
}

# `sudo` keeps the terminfo (ghostty's `sudo` feature): TERM names our entry, and TERMINFO
# says where it is, so a root shell or `sudo vim` without it would find no terminal at all.
# sudoedit (`-e`, `--edit`) takes no --preserve-env and is left alone.
if [[ -n "${TERMINFO-}" ]]; then
    sudo() {
        'builtin' 'local' arg edit=0
        for arg in "$@"; do
            if [[ "$arg" == -e || "$arg" == --edit ]]; then
                edit=1
                'builtin' 'break'
            fi
            [[ "$arg" == -* || "$arg" == *=* ]] || 'builtin' 'break'
        done
        if (( edit )); then
            'builtin' 'command' sudo "$@"
        else
            'builtin' 'command' sudo --preserve-env=TERMINFO "$@"
        fi
    }
fi

# `claude` starts the user's own Claude Code wired as an agent Slopty starts: its hooks reach
# the worker (status, permission prompts), it has Slopty's tools when the worker has a server,
# and it loads Slopty's mod. `slopty hook wire` says how, as NUL-ended words: the variables to
# set, an empty word, then the arguments, the user's own among them. Without that answer (an
# older CLI, an error) the call goes through as typed. A `claude` of the user's own (an alias
# or a function) is left alone, and SLOPTY_NO_CLAUDE_MOD=1 passes every call through untouched.
if [[ -n "${SLOPTY_CLI-}" && -x "$SLOPTY_CLI" ]] && (( ! $+functions[claude] && ! $+aliases[claude] )); then
    function claude {
        if [[ -n "${SLOPTY_NO_CLAUDE_MOD-}" && "$SLOPTY_NO_CLAUDE_MOD" != 0 ]]; then
            'builtin' 'command' claude "$@"
            return
        fi
        'builtin' 'local' _slopty_word
        'builtin' 'local' -i _slopty_args=0
        'builtin' 'local' -a _slopty_env _slopty_argv
        while IFS= 'builtin' 'read' -r -d '' _slopty_word; do
            if (( _slopty_args )); then
                _slopty_argv+=("$_slopty_word")
            elif [[ -z "$_slopty_word" ]]; then
                _slopty_args=1
            else
                _slopty_env+=("$_slopty_word")
            fi
        done < <("$SLOPTY_CLI" hook wire -- "$@" 2>/dev/null)
        if (( ! _slopty_args )); then
            'builtin' 'command' claude "$@"
            return
        fi
        (( ${#_slopty_env} )) && 'builtin' 'local' -x "${_slopty_env[@]}"
        'builtin' 'command' claude "${_slopty_argv[@]}"
    }
fi

# `ssh` keeps a terminal the far side knows (ghostty's `ssh-env` and `ssh-terminfo`): `slopty
# ssh` installs our terminfo entry on the host once, else the session gets xterm-256color, and
# SLOPTY_NO_SSH_TERMINFO=1 leaves every host untouched. An `ssh` of the user's own (an alias or
# a function) is left alone, and `command ssh` is always the plain one.
if [[ -n "${SLOPTY_CLI-}" && -x "$SLOPTY_CLI" ]] && (( ! $+functions[ssh] && ! $+aliases[ssh] )); then
    function ssh {
        "$SLOPTY_CLI" ssh -- "$@"
    }
fi
