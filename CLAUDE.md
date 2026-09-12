# Slopty

A remote-coding app: terminals and Parsec-class remote desktop from many clients to one host,
on an infinite canvas, with Claude Code driven in place. macOS and iOS, pure Rust on GPUI.

Maps: `docs/ARCHITECTURE.md` (how it is built), `docs/decisions/` (rulings with their
evidence; `docs/DECISIONS.md` is the index), `docs/MEASUREMENTS.md` (numbers and the commands
behind them), `docs/DEV.md` (commands and the loop), `docs/TESTING.md` (the four test layers).
`docs/knowledge-from-slop-desk.md` is unverified prior art.

## What must hold
- Pure Rust, everywhere: the app, the daemons, and every script (`cargo xtask …`).
- Floor macOS 26.5 / iOS 26.5, Apple silicon; no availability checks, no fallbacks.
- Latest stable toolchain and dependencies; the strictest lints, all of them on. An `#[allow]`
  carries a `reason`; `[workspace.lints]` is never loosened to make something build.
- A commit lands only on a green `cargo gate`, with a Conventional Commits message
  (`committed` lints it; releases derive from them, so `CHANGELOG.md` and the version are
  never edited by hand).
- Every `unsafe` block states the framework or ABI rule it relies on. Apple constants come from
  the objc2 statics; a `CFSTR` macro constant is spelled once, next to its use, naming its header.
- Wire types live in `slopty-proto` with insta goldens; a changed golden is a protocol change
  and bumps `PROTOCOL_VERSION`.
- Libraries hold no `std::sync::Mutex`, no `thread::sleep`, no `unwrap`/`expect`.
- Optimisations follow a measurement, recorded in `docs/MEASUREMENTS.md`.
- Every behaviour has a test at the lowest layer that can see it (`docs/TESTING.md`); a
  decision worth a paragraph gets its entry under `docs/decisions/` in the same change.

## Session safety
- Checks run as tests, never by hand: no synthetic keys into a pid, no screenshots of other
  windows, no reading images back (that pattern reads as surveillance tooling and has been
  flagged). Goldens are compared numerically; read the diff numbers, never open the image.
- Old Claude Code transcripts (`~/.claude/projects/**/*.jsonl`) are never read, grepped or
  dumped into a session; a flag is investigated through metadata only.
