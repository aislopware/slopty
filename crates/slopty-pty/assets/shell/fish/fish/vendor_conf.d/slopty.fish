# Slopty shell integration for fish: OSC 133 prompt marks, so the client can draw command
# blocks, jump between prompts, copy a command's output and tint a failed command.
#
# Loaded because slopty-ptyd prepends its data dir to XDG_DATA_DIRS (fish reads
# <dir>/fish/vendor_conf.d/*.fish from every entry). fish 4.0 and later emit the marks on
# their own (A;click_events=1 / B / C;cmdline_url= / D;<status>, ST-terminated), so on those
# this file only records that it loaded. On fish 3, vendor snippets run before config.fish, so
# the prompt is wrapped on the first fish_prompt event, after the user's prompt is defined:
#
#   133;A / 133;B  around fish_prompt's output
#   133;C          on fish_preexec
#   133;D;<status> on fish_postexec, only when a C is open
#
# Written by slopty-ptyd on every start; edits here are lost. Opt out with
# SLOPTY_NO_SHELL_INTEGRATION=1.

if status is-interactive; and not set -q __slopty_integrated
    set -g __slopty_integrated 1
end

if status is-interactive; and test "$__slopty_integrated" = 1; and not string match -rq '^[4-9]\.' -- $version
    set -g __slopty_integrated wrapped

    function __slopty_mark
        printf '\e]133;%s\a' $argv[1]
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
            __slopty_mark A
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

# The working directory (OSC 7) before each prompt, so the client can name where a shell is.
if status is-interactive
    function __slopty_cwd --on-event fish_prompt
        set -l dir (string replace -a % %25 -- $PWD | string replace -a ' ' %20)
        printf '\e]7;file://%s%s\a' $hostname $dir
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
