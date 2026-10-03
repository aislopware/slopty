# Slopty shell integration for fish: OSC 133 prompt marks, so the client can draw command
# blocks, jump between prompts, copy a command's output and tint a failed command.
#
# Loaded because slopty-ptyd prepends its data dir to XDG_DATA_DIRS (fish reads
# <dir>/fish/vendor_conf.d/*.fish from every entry). fish 4.0 and later emit the marks on
# their own (A;click_events=1 / B / C;cmdline_url= / D;<status>, ST-terminated), so on those
# this file only says, once, that fish redraws its whole prompt after a resize (an A with
# `redraw=1` ahead of the first prompt's own, on the same fresh line): the terminal then clears
# the prompt first, as a terminal assuming kitty's default does. On fish 3, vendor snippets run
# before config.fish, so the prompt is wrapped on the first fish_prompt event, after the user's
# prompt is defined:
#
#   133;A / 133;B  around fish_prompt's output (A with `redraw=1`)
#   133;C          on fish_preexec
#   133;D;<status> on fish_postexec, only when a C is open
#
# The working directory goes as OSC 7 before each prompt, percent-encoded.
#
# In a tmux pane every mark goes through tmux's passthrough (DCS `tmux;`, each ESC doubled),
# since tmux keeps OSC 133 and OSC 7 to itself, and the pane allows it (`allow-passthrough`,
# off by default since tmux 3.3; `on` passes only while the pane is visible, which is when its
# marks match the screen Slopty draws). fish 4's own marks are kept by tmux too, so there the
# prompt is wrapped as on fish 3.
#
# Written by slopty-ptyd on every start; edits here are lost. Opt out with
# SLOPTY_NO_SHELL_INTEGRATION=1.

# A tmux pane is no Slopty tile: its server may have started in another session, whose
# presence file (held while a client looks at that tile) would silence this pane's agents.
if set -q TMUX
    set -e CLAUDE_CLIENT_PRESENCE_FILE
end

if status is-interactive; and not set -q __slopty_integrated
    set -g __slopty_integrated 1
end

# How an OSC starts and ends here. A shell ptyd started is told TERM_PROGRAM=slopty, and tmux
# sets its own in a pane, so a TMUX inherited from a daemon that ran under tmux does not count.
if test -n "$TMUX"; and test "$TERM_PROGRAM" != slopty
    set -g __slopty_osc \e'Ptmux;'\e\e']'
    set -g __slopty_st \a\e'\\'
    if status is-interactive; and test "$__slopty_integrated" = 1
        command tmux set-option -p allow-passthrough on >/dev/null 2>&1
    end
else
    set -g __slopty_osc \e']'
    set -g __slopty_st \a
end

if status is-interactive; and test "$__slopty_integrated" = 1; and begin
        not string match -rq '^[4-9]\.' -- $version; or test "$__slopty_osc" != \e']'
    end
    set -g __slopty_integrated wrapped

    function __slopty_mark
        printf '%s133;%s%s' $__slopty_osc $argv[1] $__slopty_st
    end

    function __slopty_wrap_prompt --on-event fish_prompt
        # Once: the user's config.fish has run by now, so fish_prompt is the one to keep.
        functions -e __slopty_wrap_prompt
        if functions -q fish_prompt
            functions -c fish_prompt __slopty_user_prompt
        else
            function __slopty_user_prompt
                printf '> '
            end
        end
        function fish_prompt
            __slopty_mark 'A;redraw=1'
            __slopty_user_prompt
            __slopty_mark B
        end
    end

    function __slopty_preexec --on-event fish_preexec
        set -g __slopty_running 1
        __slopty_mark C
    end

    function __slopty_postexec --on-event fish_postexec
        set -l __slopty_status $status
        if set -q __slopty_running
            set -e __slopty_running
            __slopty_mark "D;$__slopty_status"
        end
    end
end

if status is-interactive; and test "$__slopty_integrated" = 1
    function __slopty_redraw --on-event fish_prompt
        functions -e __slopty_redraw
        printf '\e]133;A;redraw=1\a'
    end
end

# Slopty's `open`, BROWSER and EDITOR first on the path (SLOPTY_BIN) before each prompt: the
# user's config may have put the system's `open` ahead of it.
if status is-interactive; and set -q SLOPTY_BIN
    function __slopty_path --on-event fish_prompt
        if test "$PATH[1]" != "$SLOPTY_BIN"
            set -gx PATH $SLOPTY_BIN (string match -v -- $SLOPTY_BIN $PATH)
        end
    end
end

# The working directory (OSC 7) before each prompt, so the client can name where a shell is.
if status is-interactive
    function __slopty_cwd --on-event fish_prompt
        set -l dir (string escape --style=url -- $PWD)
        printf '%s7;file://%s%s%s' $__slopty_osc $hostname $dir $__slopty_st
    end
end

# `sudo` keeps the terminfo (ghostty's `sudo` feature): TERM names our entry, and TERMINFO
# says where it is, so `sudo vim` without it would find no terminal at all. sudoedit
# (`-e`, `--edit`) takes no --preserve-env and is left alone; a sudo that is already a
# function or an alias is the user's.
if status is-interactive; and test -n "$TERMINFO"; and test file = (type -t sudo 2>/dev/null; or echo x)
    function sudo -d "sudo, keeping TERMINFO"
        set -l edit no
        for arg in $argv
            if test "$arg" = -e; or test "$arg" = --edit
                set edit yes
                break
            end
            if not string match -rq -- '^-' "$arg"; and not string match -rq -- '=' "$arg"
                break
            end
        end
        if test "$edit" = yes
            command sudo $argv
        else
            command sudo --preserve-env=TERMINFO $argv
        end
    end
end

# `claude` starts the user's own Claude Code wired as an agent Slopty starts: its hooks reach
# the worker (status, permission prompts), it has Slopty's tools when the worker has a server,
# and it loads Slopty's mod. `slopty hook wire` says how, as NUL-ended words: the variables to
# set, an empty word, then the arguments, the user's own among them. Without that answer (an
# older CLI, an error) the call goes through as typed. A `claude` of the user's own (a
# function, an alias, an autoloaded file) is left alone, and SLOPTY_NO_CLAUDE_MOD=1 passes
# every call through untouched.
if status is-interactive; and set -q SLOPTY_CLI; and test -x "$SLOPTY_CLI"; and not functions -q claude
    function claude --wraps claude -d "claude, wired to Slopty"
        if test -n "$SLOPTY_NO_CLAUDE_MOD"; and test "$SLOPTY_NO_CLAUDE_MOD" != 0
            command claude $argv
            return
        end
        set -l words ($SLOPTY_CLI hook wire -- $argv 2>/dev/null | string split0)
        set -l split (contains -i -- "" $words)
        if test -z "$split"
            command claude $argv
            return
        end
        set -l vars $words[1..$split]
        set -e vars[-1]
        set -e words[1..$split]
        # `env` sets them for this one run in every fish 3: a `set -lx` would end with the loop
        # that made it, and `set -f` needs fish 3.4. It hands over to claude as typing it does.
        if set -q vars[1]
            command env $vars claude $words
        else
            command claude $words
        end
    end
end

# `codex` runs without what names this terminal (SLOPTY_SESSION, its token, SLOPTY_PROJECT and
# SLOPTY_TASK). Codex's app-server daemon, which the first `codex` starts, keeps its starter's
# environment for every thread's commands, so a daemon started here would have every Codex
# thread, whoever started it, speak as this terminal. A `codex` of the user's own (a
# function, an alias, an autoloaded file) is left alone.
if status is-interactive; and set -q SLOPTY_SESSION; and not functions -q codex
    function codex --wraps codex -d "codex, as no Slopty terminal"
        command env -u SLOPTY_SESSION -u SLOPTY_SESSION_TOKEN -u SLOPTY_PROJECT -u SLOPTY_TASK \
            codex $argv
    end
end

# `ssh` keeps a terminal the far side knows (ghostty's `ssh-env` and `ssh-terminfo`): `slopty
# ssh` installs our terminfo entry on the host once, else the session gets xterm-256color, and
# SLOPTY_NO_SSH_TERMINFO=1 leaves every host untouched. An `ssh` of the user's own (a function,
# an alias, an autoloaded file) is left alone, and `command ssh` is always the plain one.
if status is-interactive; and set -q SLOPTY_CLI; and test -x "$SLOPTY_CLI"; and not functions -q ssh
    function ssh --wraps ssh -d "ssh, with a terminal the far side knows"
        $SLOPTY_CLI ssh -- $argv
    end
end
