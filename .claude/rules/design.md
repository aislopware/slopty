---
paths:
  - "crates/slopty-shell/**"
  - "crates/slopty-shell-theme/**"
  - "crates/slopty-terminal/**"
  - "crates/slopty-ui/**"
  - "crates/slopty-app/**"
  - "crates/slopty-e2e/golden/**"
  - "apps/slopty/**"
  - "apps/slopty-ios/**"
---

# Design

The window is a terminal, not an app with a terminal in it (`docs/decisions/layout.md`). The bar
is the premium terminals: Rex first, then Ghostty, herdr, iTerm2, WezTerm, kitty, Blink and Prompt;
the studies in `.research/terminal-first-2026-10-30/` name their values. GUI-first apps (zeron,
Claude Code desktop, T3 Code, opencode, Amp) are no longer the reference for layout or components.
TUI-first (`docs/decisions/agents.md`): a session's terminal is the main surface and nothing sits
over it; the inspector supports it, read-only, quiet, and closed until summoned.

- One row of chrome; one state vocabulary on every surface; a stable list whose marks carry the
  attention; views summoned beside the terminal, never a permanent column.
- Colour is never muted into grey; a hue changes only to one a reference shows is better. Nothing
  Slopty draws may read as a seam.
- Slopty's data reaches the shell only through its view model (`slopty_ui::view_model`).
- Look at the renders, not only the diff: a golden can encode a defect its numbers never see.
