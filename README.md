# Slopty

One app on macOS, iPhone and iPad that reaches many Macs at once. It runs terminals and
Claude Code agents on them, streams their windows and desktops, shares the clipboard and moves
files either way, all on one scrolling workspace. The aim is that working on a remote Mac feels
local.

Slopty adds no encryption or pairing of its own: reach your machines over Tailscale or a VPN,
which is the security boundary. A machine admits the tailnet and loopback by default; `[worker]
allow` in `settings.toml` adds a private LAN or VPN.

## How it fits together

- **Machines** (every Mac or Linux box you want to reach) run the terminals, the agents and the
  capture. On each one that is `slopty-worker`, kept running by launchd or systemd.
- **The server** (one per setup, on any machine) keeps the list of machines and serves the tools
  to agents.
- **Clients** (the app on each Mac, iPhone or iPad, and the `slopty` CLI) ask the server who is
  there, then talk to each machine directly.

One Mac can play all three roles.

## Install

Requires macOS 26.5 or later on Apple silicon. Download `Slopty-<version>-macos-arm64.zip` from
[Releases](https://github.com/aislopware/slopty/releases), unzip it and move `Slopty.app` to
Applications. Slopty offers the move itself when it is opened from somewhere else, because the
machine it sets up runs from the app.

Then, in the app:

- **Use this Mac** shares this Mac's shells and windows and adds it, with a checklist for the two
  permissions it needs: Screen Recording and Accessibility.
- **Install on a machine over SSH** copies Slopty to another Mac or a Linux machine you reach
  with `ssh` (your ssh config and agent; a machine that takes only a password asks for it once
  and can take your key so it stops asking), then adds it.
- **Connect to a server** with a server's Tailscale name or address: every machine registered
  with it shows up by itself, on every client. **Run the server on this Mac** or **Set up the
  server over SSH** puts one up. Machines added before the server joins it through **Register
  machines with the server** in the palette.
- **Add a machine** reaches one by address, without a server.

A new build of the app updates this Mac's own machine and server by itself. Any other machine or
server on an older build shows **Update**. **Update all machines** in the palette updates the
server first, then every machine, and a machine that was away is updated when it comes back. No
machine is ever taken back to an older build. An update keeps every shell and agent turn running,
and asks first in the rare build that cannot.

A headless Mac runs Slopty in the session of whoever is logged in, so set it to log in
automatically (with FileVault off), or unlock it over `ssh` and log in through Screen Sharing
after a restart. A Linux machine keeps running after you log out once
`loginctl enable-linger` is on for your user; Slopty says when it is not.

### From the command line

The bundle carries the CLI, so a machine can be set up without the app:

```sh
Slopty.app/Contents/MacOS/slopty server install                 # on the server
Slopty.app/Contents/MacOS/slopty worker install --server <host> # on each Mac to reach
slopty --server <host> worker deploy <ssh-host>                 # a Mac or Linux box over ssh
```

To build it yourself, `cargo xtask bundle` writes `target/bundle/Slopty.app`.

## Agents

Claude Code started in a Slopty terminal, whether from the app or typed as `claude` in a shell,
gets Slopty's tools (list machines, open terminals, type, read screens, wait on commands, read
and write files) and reports whether it is working, waiting or blocked, with nothing to set up.
Your own `claude` runs unmodified, with your flags; `SLOPTY_NO_CLAUDE_MOD=1` leaves it alone.

For Claude Code started elsewhere, add the tools yourself:

```sh
claude mcp add slopty -- slopty mcp
```

and `slopty hook install` registers the hooks that report whether it is working, waiting or
blocked.

## The CLI

`slopty --help` lists everything. The everyday verbs:

| Verb | What it does |
| --- | --- |
| `workers` | The machines the server knows, whether they are up, and the agents waiting on you |
| `terminals` | Terminals on one machine or all of them |
| `open` / `close` | Start or hang up a terminal |
| `send` / `screen` / `output` | Type into a terminal, print its screen, print its scrollback |
| `wait` / `commands` | Block until something happens; the commands run and their exit codes |
| `cat` / `put` | Read a file on a machine, or replace one from standard input |
| `ports` | TCP ports listening in a machine's terminals |
| `attach` | A raw terminal straight to a machine, detached with `^]` |

The server comes from `--server`, `$SLOPTY_SERVER`, or `[client] server` in `settings.toml`.

## Working on Slopty

`CLAUDE.md` holds the rules. `docs/DEV.md` covers the commands and the loop,
`docs/ARCHITECTURE.md` how it is built, `docs/TESTING.md` the test layers, and
`docs/DECISIONS.md` the rulings with their evidence.
