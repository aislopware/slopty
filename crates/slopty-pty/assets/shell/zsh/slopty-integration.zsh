# Slopty shell integration for zsh: OSC 133 prompt marks, so the client can draw command
# blocks, jump between prompts, copy a command's output and tint a failed command.
#
#   133;A  prompt starts (put in PS1, so a theme that rebuilds the prompt keeps it)
#   133;B  input starts (end of PS1)
#   133;C  output starts (preexec)
#   133;D;<status>  command ended (precmd, before the next prompt)
#
# Sourced by the bootstrap .zshenv for interactive shells; safe to source twice.

[[ -n "${_slopty_integrated+set}" ]] && return 0
'builtin' 'typeset' -g _slopty_integrated=1
# 1 while a command runs (a C mark is open and needs its D).
'builtin' 'typeset' -gi _slopty_running=0

_slopty_precmd() {
    'builtin' 'local' -i _slopty_last=$?
    if (( _slopty_running )); then
        'builtin' 'print' -n -- $'\e]133;D;'"$_slopty_last"$'\a'
        _slopty_running=0
    fi
    # Marks inside PS1/PS2 so zle redraws keep them; %{ %} hides them from width counting.
    if [[ "$PS1" != *$'\e]133;A'* ]]; then
        PS1=$'%{\e]133;A\a%}'"$PS1"$'%{\e]133;B\a%}'
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
    'builtin' 'print' -n -- $'\e]133;C\a'
    _slopty_running=1
}

'builtin' 'typeset' -ga precmd_functions preexec_functions
precmd_functions+=(_slopty_precmd)
preexec_functions+=(_slopty_preexec)
