# Slopty shell integration, zsh bootstrap.
#
# slopty-ptyd points ZDOTDIR at this directory only so that zsh reads this file first. It hands
# ZDOTDIR back to the user (SLOPTY_ZSH_ZDOTDIR holds the original, if there was one), sources
# the user's own .zshenv from there, and for interactive shells loads slopty-integration.zsh.
# The user's .zprofile, .zshrc and .zlogin then load from their usual place, untouched.
#
# Written by slopty-ptyd on every start; edits here are lost. Opt out with
# SLOPTY_NO_SHELL_INTEGRATION=1 in the daemon's environment.

if [[ -n "${SLOPTY_ZSH_ZDOTDIR+set}" ]]; then
    'builtin' 'export' ZDOTDIR="$SLOPTY_ZSH_ZDOTDIR"
    'builtin' 'unset' 'SLOPTY_ZSH_ZDOTDIR'
else
    'builtin' 'unset' 'ZDOTDIR'
fi

{
    'builtin' 'typeset' _slopty_rc="${ZDOTDIR-$HOME}/.zshenv"
    [[ ! -r "$_slopty_rc" ]] || 'builtin' 'source' '--' "$_slopty_rc"
} always {
    if [[ -o 'interactive' ]]; then
        'builtin' 'typeset' _slopty_rc="${${(%):-%x}:A:h}/slopty-integration.zsh"
        [[ ! -r "$_slopty_rc" ]] || 'builtin' 'source' '--' "$_slopty_rc"
    fi
    'builtin' 'unset' '_slopty_rc'
}
