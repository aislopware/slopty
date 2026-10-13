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

The interface is zeron's look and behaviour, through Slopty's copy of its shell
(`crates/slopty-shell`; `docs/decisions/ui.md`, "The interface is zeron's UI and UX, copied as
Slopty's own"). The spec is zeron's own code: when unsure, do what it does. Nothing Slopty draws
may read as a seam.

- Change the shell only by a minimal edit marked `// slopty:`; zeron's improvements arrive by
  `cargo xtask upstream adopt zeron` through `xtask/zeron.toml`.
- Slopty's data reaches the shell only through its view model (`slopty_ui::view_model`).
- Look at the renders, not only the diff: a golden can encode a defect its numbers never see.
