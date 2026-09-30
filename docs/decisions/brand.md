# Decisions — Brand

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **Slopty's mark is a prompt on the aislopware grid, `>` with its cursor dot, in green:
  OKLCH 0.72 0.16 150, `#4ac06c`** (2026-09-30, verified by computing it). The user asked for a
  mark that belongs to the aislopware family and for green as Slopty's colour. Slopty is a
  terminal, and its connection should always look healthy, never like the yellow or red of a
  warning.
  - **The family** (`aislopware-brand/README.md`, `family-palette.json`) is a 3×3 grid of
    circles whose lit dots spell each product's letter. The org lights an A and slopscale an S.
    Unlit dots stay in the mark at fill-opacity 0.3, or 0.2 on the ink plate. Every product
    gets one hue at OKLCH L 0.72, C 0.16, with hues at least 50° apart.
  - **Slopty shows a glyph, not a letter: a prompt waiting for input**, `#../.#./#.#`. The
    chevron `>` is lit at (column, row) (0,0), (1,1) and (0,2), and the cursor is the lit dot at
    (2,2), on the baseline after it. It bends the family's letter rule on purpose; the other
    rules hold. Every dot is a circle of the same size, not all nine are lit, and unlit is
    opacity. The cursor is its own element (`id="cursor"`) in every SVG and its own glass group
    in the icon, so the app can drive it apart from the rest.
  - **Rejected.** A **T** (`###/.#./.#.`): the user found it did not read as a terminal.
    **Capsule cursors** in cell (2,2) in place of a dot: an underscore, a wide underscore, a
    block and a beam. They read more literally as a cursor, but they break the all-circles
    rule the user wants kept for the family. The candidate renders also showed that a block
    cursor turns into a dot at 16 px anyway, and that a beam is too thin to hold.
  - **In the app, the cursor is the live element** (built 2026-09-30: `slopty-ui`
    `workspace::about::Mark`, `docs/decisions/ui.md`, "Slopty's mark in the app"). Where the
    app shows the mark (the empty workspace, the About panel), the cursor dot blinks at the
    terminal's cursor rate. It holds steady under Reduce Motion. Lit means connected, and the
    cursor is the unlit level while no worker is reachable. The dock icon stays static, since
    app icons do not animate.
  - **The green slot is hue 150.** It belonged to slop-desk, Slopty's retired predecessor,
    which only used green as its chart colour. The brand README now lists slop-desk as retired
    and succeeded by Slopty. The nearest family hues are the org's 55 and slopscale's 240, both
    more than 50° away.
  - **The hue reads as clean green.** Tailwind v4, whose L 0.72 rule the family follows,
    names these hues (`theme.css`):

    | | lime-500 | green-500 | emerald-500 | teal-500 |
    |---|---|---|---|---|
    | Hue | 130.9 | 149.6 | 162.5 | 182.5 |

    Hue 150 is green-500's. Lower hues drift toward lime and yellow, the caution colour, and
    higher ones toward mint.
  - **Gamut and contrast** at L 0.72, C 0.16. The ceiling is the highest chroma sRGB holds at
    that lightness and hue:

    | Hue | Ceiling | Hex | On ink `#1c1f26` | On paper `#f2f1ec` | On white |
    |---|---|---|---|---|---|
    | 130 | 0.194 | `#83b83f` | 6.98 | 2.09 | 2.36 |
    | 140 | 0.232 | `#69bc57` | 7.01 | 2.08 | 2.35 |
    | 145 | 0.227 | `#5bbe62` | 7.06 | 2.07 | 2.34 |
    | **150** | **0.198** | **`#4ac06c`** | **7.11** | **2.05** | **2.32** |
    | 155 | 0.178 | `#35c177` | 7.10 | 2.05 | 2.32 |
    | 160 | 0.163 | `#12c281` | 7.11 | 2.05 | 2.32 |
    | 165 | 0.151 | out of gamut | | | |

    At hue 150, C 0.16 has headroom. At 160 it is at the edge (red clips to `12`), and past
    that the colour leaves sRGB. On ink, an unlit dot at 0.2 composites to `#253f34`, 1.44:1
    against the plate: it stays visible without competing with the prompt. The test recomputes
    the hex from OKLCH (`icon::tests::the_art_is_a_prompt_with_its_cursor_in_slopty_green`).
  - The brand repo has `slopty-mark.svg`, `slopty-avatar.svg`/`.png` (920 px),
    `slopty-lockup.svg`, and a resvg tool (`render/`) that renders its PNG files, `family.png`
    included. slopscale's published `#2caff9` is not what its OKLCH 0.72 0.16 240 converts to
    (`#19affe`). That is recorded here and left to slopscale.

- ✅ **The app icon is an Icon Composer document generated from `assets/icon.svg` and compiled
  by `actool`** (2026-09-30, verified on this Mac with Xcode 27.0, actool and ictool 27.0).
  The icon is the green prompt on the ink plate. The system masks it and gives it Liquid Glass,
  plus its dark, tinted and clear looks.
  - **What the platforms require.**
    - Canvas and mask: every platform but watchOS takes a 1024 px square with square layers
      and applies the rounded rectangle itself. On macOS the mask sits at 824 inside 1024,
      with a rounder corner than macOS 11 had
      ([HIG, App icons](https://developer.apple.com/design/human-interface-guidelines/app-icons);
      [WWDC25 220](https://developer.apple.com/videos/play/wwdc2025/220/)).
    - Liquid Glass comes only from an Icon Composer `.icon`: layers as SVG or PNG, at most four
      groups
      ([Creating your app icon using Icon Composer](https://developer.apple.com/documentation/Xcode/creating-your-app-icon-using-icon-composer)).
    - Dark and tinted from an asset catalog exist only for iOS
      ([Configuring your app icon](https://developer.apple.com/documentation/xcode/configuring-your-app-icon)).
      On macOS, actool drops appiconset appearance variants as "unassigned children".
    - A legacy `.icns` is masked when its art is full bleed or already the squircle. It is
      shrunk onto a grey plate only when its pixels stick out past the mask or its edges are
      transparent. We measured this through the system's icon lookup on macOS 27.0.1.
      [Developer](https://lapcatsoftware.com/articles/2025/6/2.html)
      [reports](https://www.heise.de/en/news/Icons-in-macOS-26-Fighting-the-Squircle-Prison-11075561.html)
      agree; Apple does not document the trigger.

    So a resvg-only `.icns` would avoid the plate, but it would be flat, and on macOS it would
    have no dark, tinted or clear variant.
  - **Route.**
    - `assets/icon.svg` stays the one source: a full-bleed ink `#plate`, eight
      `<circle id="dot-c-r">` and `<circle id="cursor">`. `xtask/src/icon.rs` parses it with
      usvg and holds it to the exact mark. It refuses a dot lit outside the prompt, an unlit
      cursor, a cursor that is not its own element, a shape that is not a circle (a capsule or
      a rounded square), a dot off the grid or of another size, and a plate that is not full
      bleed.
    - From that it writes `AppIcon.icon`, with one SVG layer per dot so the glass gives each
      dot its own depth.
    - `actool` compiles the document. It ships with Xcode and belongs to the platform
      toolchain, like the linker. `cargo xtask bundle` runs it into `Contents/Resources`, which
      gets `Assets.car` plus a fallback `AppIcon.icns` at 16 to 256 px. `Info.plist` names both
      (`CFBundleIconName`, `CFBundleIconFile`).
    - `cargo xtask ios` puts the document in the XcodeGen project. XcodeGen types it
      `wrapper.icon`, `ASSETCATALOG_COMPILER_APPICON_NAME` names it, and xcodebuild compiles it.
      The appiconset and its 1024 PNG are gone.
    - Rejected: authoring the icon in the Icon Composer app, which would make a second source
      of truth that no test can see; the flat `.icns`, for the reasons above.
  - **Layer settings, measured with `ictool`.** `ictool` is Icon Composer's own renderer
    (`Icon Composer.app/Contents/Executables/ictool`), and it renders every appearance.
    - The chevron and the cursor are glass groups of their own (so the cursor can be tinted
      or driven alone later), each with individual lighting and a layer-colour shadow at 0.5.
    - **Specular off, translucency off.** With specular on, the face of a lit dot measures
      `#6cbd74` instead of `#4ac06c`: a pastel wash plus a dark rim. With it off, the face
      measures `#4abf6d`, the brand green.
    - The unlit dots form a flat group at layer opacity 0.2.
    - The fill is solid ink.
    - **Tinted appearances fill every dot white** (`fill-specializations`). Tinting keeps
      luminance, so green dots came out `#47337c` on `#241f2f` in tinted dark; white makes them
      `#6143b5`.
    - **Every layer's colour is its own sRGB-tagged fill** (`fill-specializations`, default
      `srgb:0.29020,0.75294,0.42353`), not the SVG's untagged hex. Xcode versions disagree on
      what an untagged SVG colour means. Xcode 27 reads it as sRGB, and its one explicit
      alternative, `color-space-for-untagged-svg-colors: display-p3`, renders `#4ac06c` as P3
      numbers (`#00c362` in sRGB). CI's default Xcode 26.6 rendered the lit dots away from the
      brand green (2026-09-30, run 36669257345). With the tagged fill the faces measure
      `#4ac06d` on Xcode 27.0 and 27.1.
    - **ictool's PNG files are colour-managed when read.** It writes whatever space a render
      needed: 8-bit sRGB for most, but 16-bit Display P3 with an embedded ICC profile for
      TintedLight even on Xcode 27. The test decodes at full depth and converts through the
      embedded matrix/TRC profile to sRGB (`icon::tests::colour`). A failure prints each lit
      dot's colour and the profile.
  - **Geometry.** The grid spans 68 % of the plate (pitch 232) and each dot is 0.38 of the
    pitch across (r 88), the avatar's proportions. No small-size tuning was needed. At 16 px
    the pitch is 2.9 px, so each dot lands on about a 2×2 pixel block: lit 187, unlit about
    60, ink 31 in the green channel, with the gaps back at ink.
  - **The prompt reads everywhere, and the cursor reads on its own from 32 px**
    (`icon::tests::compiles_and_the_prompt_reads_at_every_size_and_appearance`). The test samples
    each dot's centre (WCAG contrast). It compares the dimmest lit dot with the brightest
    unlit one (the prompt), and the cursor with its unlit neighbour on the baseline.
    - actool's `.icns` slots: prompt 5.0:1 and cursor 5.0:1, except 3.30:1 (prompt) in the
      1× 16 px slot. Unlit dots are 1.44:1 over the plate.
    - ictool's Default at 16 to 1024 px: prompt 4.67 to 4.81:1, cursor 4.67 to 4.93:1 (4.74:1
      at 32 px).
    - Appearances at 128 px (prompt / cursor): Default 4.67 / 4.67, Dark 4.88 / 5.40,
      TintedLight 3.46 / 3.46, TintedDark 1.98 / 2.08, ClearLight 2.79 / 2.79,
      ClearDark 6.55 / 6.55.
    - **The lit dots are the brand green within one just-noticeable difference** (Oklab ΔE
      0.02) at 128 px and up, in ictool's renders and in the shipped `.icns` slots. Xcode 27's
      glass shading puts them at 0.006 to 0.012. The specular wash (0.032) and the hexes read as
      P3 (0.030) fall outside. At 1024 px, 9.18 % of the pixels are within that distance; the
      four lit dots' area is 9.28 %.

    Rerun: `cargo nextest run -p xtask icon --no-capture`. The review renders
    (`icon-{16,32,128,512,1024}.png` and the five other appearances at 512 px) come from
    `cargo xtask icon target/e2e/artifacts`.
