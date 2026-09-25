# Slopty

One app on macOS, iPhone and iPad that reaches many Macs at once. It runs terminals and
Claude Code agents on them, streams their windows and desktops, shares the clipboard and moves
files either way, all on one scrolling workspace. The aim is that working on a remote Mac feels
local.

Slopty adds no encryption or pairing of its own: reach your machines over Tailscale or a VPN,
which is the security boundary. Workers admit the tailnet and private LANs by default
(`[worker] allow` in `settings.toml` narrows that).

## How it fits together

- **The server** (one per setup) keeps the directory of workers and serves the tools to agents.
- **Workers** (every Mac you want to reach) run the terminals and the capture, and register
  with the server.
- **Clients** (the app on each Mac, iPhone or iPad, and the `slopty` CLI) ask the server who
  is there, then talk to each worker directly.

One machine can play all three roles.

## Quick start

Requires macOS 26.5 or later on Apple silicon.

```sh
cargo xtask bundle            # target/bundle/Slopty.app: the app, the daemons and the CLI
```

On the machine that will be the server:

```sh
Slopty.app/Contents/MacOS/slopty server install
```

On every Mac that should be reachable (the server can be one of them):

```sh
slopty=Slopty.app/Contents/MacOS/slopty
$slopty settings init      # writes settings.toml; `settings path` prints where
# set `server = "<server-host>"` under [worker] in that file, then:
$slopty worker install
```

The first window or desktop you stream asks for Screen Recording, and the first input you send
asks for Accessibility, on that worker.

Open `Slopty.app` on each client and choose **Connect to a server** with the server's
Tailscale name or address. The workers show up by themselves. **Add a worker** reaches one by
address without a server.

## Agents

Give Claude Code the same tools the app has (list workers, open terminals, type, read screens,
wait on commands, read and write files):

```sh
claude mcp add slopty -- slopty mcp                          # stdio, through the CLI
claude mcp add --transport http slopty http://<server-host>:45561/mcp
```

`slopty hook install` registers the hook that reports whether an agent in a Slopty terminal is
working, waiting or blocked.

## The CLI

`slopty --help` lists everything. The everyday verbs:

| Verb | What it does |
| --- | --- |
| `workers` | The workers the server knows, whether they are up, and the agents waiting on you |
| `terminals` | Terminals on one worker or all of them |
| `open` / `close` | Start or hang up a terminal |
| `send` / `screen` / `output` | Type into a terminal, print its screen, print its scrollback |
| `wait` / `commands` | Block until something happens; the commands run and their exit codes |
| `cat` / `put` | Read a file on a worker, or replace one from standard input |
| `ports` | TCP ports listening in a worker's terminals |
| `attach` | A raw terminal straight to a worker, detached with `^]` |

The server comes from `--server`, `$SLOPTY_SERVER`, or `[client] server` in `settings.toml`.

## Working on Slopty

`CLAUDE.md` holds the rules. `docs/DEV.md` covers the commands and the loop,
`docs/ARCHITECTURE.md` how it is built, `docs/TESTING.md` the test layers, and
`docs/DECISIONS.md` the rulings with their evidence.
