---
paths:
  - "crates/slopty-shell/**"
  - "crates/slopty-shell-theme/**"
  - "crates/slopty-ui/**"
  - "crates/slopty-app/**"
  - "crates/slopty-e2e/golden/**"
  - "apps/slopty/**"
  - "apps/slopty-ios/**"
---

# Design

The interface starts from zeron's look and behaviour, through Slopty's copy of its shell
(`crates/slopty-shell`). Zeron is the base, not the spec (`docs/decisions/ui.md`, "Zeron is the
base; the premium references set the bar"). Where Claude Code desktop, T3 Code, opencode or Amp
do something more beautiful, elegant or striking, adopt it: the study in `.research` names the
values. When no reference is clearly better, do what zeron does. Colour is never muted into grey;
a hue changes only to one a reference shows is better. Nothing Slopty draws may read as a seam.

- Change the shell only by a minimal edit marked `// slopty:`; zeron's improvements arrive by
  `cargo xtask upstream adopt zeron` through `xtask/zeron.toml`.
- Slopty's data reaches the shell only through its view model (`slopty_ui::view_model`).
- Look at the renders, not only the diff: a golden can encode a defect its numbers never see.
