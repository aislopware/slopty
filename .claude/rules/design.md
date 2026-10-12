---
paths:
  - "crates/slopty-ui/**"
  - "crates/slopty-kit/**"
  - "crates/slopty-theme/**"
  - "crates/slopty-app/**"
  - "crates/slopty-e2e/golden/**"
  - "apps/slopty/**"
  - "apps/slopty-ios/**"
---

# Design

The bar: someone who works in Zed and Delta (delta.dev, Zed Industries' agent app) all day sees
no seam. Zed sets the design system and components; Delta sets the thread, sidebar, review and
composer. Square, technical and very modern; minimal, never plain.

- The rulings and measured values: `.research/zed-clone-2026-10-11.md`, then
  `.research/design-polish-2026-10-12.md`, `.research/delta-re-2026-10-12.md` and
  `.research/delta-signed-in-2026-10-12.md`, where a later file wins.
- Every colour, size and spacing comes from the theme tokens; `crates/slopty-ui/src/lints.rs`
  enforces it.
- Light and dark only, both equally premium. Solid planes, no translucency. Reduce Motion is
  honoured. Chrome text is sentence case. Keybindings live in the palette, not on buttons.
- Icons are GitHub Octicons with our own state marks, a hue per state.
- Look at the renders, not only the diff: a golden can encode a defect its numbers never see.
