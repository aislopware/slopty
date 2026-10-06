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
    app shows the mark (the empty workspace), the cursor dot blinks at the
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

- ❌ **Deleted 2026-10-05.** The companions below are gone, whole: the characters, the yard,
  the working line's and the subagents' companions, Dot beside the empty workspace's mark,
  the `[theme] companions` setting and the theme's `Companions`
  (`.research/icons-2026-10-05.md` §4.2). The person judged them tiny and ugly in a row's
  slot and out of place in the bar. A 16 pt slot cannot hold a character that says a state at
  1×, where Ember was eight pixels. In the slot it hid the one status vocabulary, so a row
  that needed the person read as one amber pixel. The yard repeated the navigator's attention
  order, and pixel art beside system type was a second drawing language. What stays is the
  mark's blinking cursor on the empty workspace. The entry is kept as history.
- ✅ ~~**Companions: a small pixel character for each agent, in its mark's place**~~ (2026-10-04,
  `.research/mascots-2026-10-04.md`; deleted 2026-10-05, above). The user asked for small playful touches: pixel-art
  characters for Claude Code, Codex, pi and the other agents that move about and play. They
  are called companions in the chrome, and `[theme] companions` sets how much they do.
  - **The characters are Slopty's own** (study §3, §4.2). Ember (Claude Code) is a round body
    under a four-point sparkle tuft on two feet, in the theme's agent orange: no arms, no
    rectangle, never four legs, so nothing of the registered Clawd design. Brace (Codex) is a
    capsule with a visor whose arms are braces, in ink. Pi is pi's own MIT mark at twice its
    size with eyes on its bar, in its three colours (`slopty_theme::PI`), the one vendor mark
    whose licence allows it. Op (`OpenCode`) is a box in its mark's frame. Any other agent is
    Blob, a dome wearing one of an open set of accessories picked by an FNV-1a hash of its name,
    so two unknown agents differ and keep their looks on every run. Dot is the brand's grid
    come alive, its cursor its one eye; it stands where no agent is. No character copies a
    vendor's mascot or logo; the hint names the agent in plain words.
  - **Sprites are code** (`slopty-ui` `companions::sprites`): eight-by-eight ASCII grids read
    by a `const fn`, so a row of the wrong width or an unknown cell fails the build. Their cells
    are ink slots (body, shade, accent, outline, eye, glint, prop, cue, muted) filled from the
    theme as they paint (`companions::Palette`). A pose is built on the stack from the body
    and transforms (sit, eyes shut or turned, a stride, a plaster, a yawn, the outline only)
    plus a prop overlay. The sixteen grid is the eight one doubled with `Scale2x`, smoothing
    only the body's own edges so a sparkle, a prop or a cue stays crisp, and pi's square blocks
    are doubled plainly; each eye gains a glint. Each row's runs of one ink, stacked into
    rectangles, are one snapped quad each, cells whole device pixels: at most 24 quads small
    and 96 large. No PNG, no atlas, no fork change.
  - **Poses follow the one status vocabulary** (study §4.3): working at its task (typing at a
    keyboard, reading a page, a little screen for a command, a thought rising, a little one
    hopping off for a subagent, ticking a list, from the newest running call's kind; a row
    that knows only its status types), waiting sat under a clock, needs you with an arm up and
    the hand in amber, to review holding a page up, done with both arms up, failed sat with
    eyes shut and a red plaster, asleep with a rising "z", silent yawning, gone as its outline
    only. The cue pixel takes the state's fill, so amber still means needs you.
  - **Where they stand.** The conversation's working line (the spinner's slot; subagents trail
    smaller after the words), the navigator's agent rows and tile rows, a tile header's and its
    tabs' slots, Dot beside the empty workspace's mark (a click is one hop; the study's second
    place for Dot, the inbox's empty line, went with the inbox), and, lively, the yard: every
    live agent's companion in the status bar beside the agent summary, needs you first, each a
    button named for who it is and what it does that goes to its tile, a count past a quarter
    of the window.
    Every place is a slot that exists, or a layer over the layout (Dot): nothing moves with
    companions on or off, and none covers content.
  - **Motion means work, and adds no frame.** A working companion steps on the working mark's
    clock (`icons::wake_at_next_step`; a frame every second step, six a second), so a window
    with an agent at work draws its twelve frames a second with companions off, quiet or
    lively. Everything else holds a pose and asks for nothing. Lively adds three moments, each
    on the clock's own step grid so it shares a frame with the mark when one runs: a wave of
    two seconds at three frames a second as an agent comes to need the person, a hop of half a
    second (six steps) as a turn ends in a tile on screen and not focused, and Dot's hop on a
    click. A moment plays only for a companion drawn before the change (GPUI's element state
    is dropped for one not drawn), so one scrolled to later never replays it. Lively play in
    the yard (blinks, a "z" rising, idle ones walking out to meet a neighbour and back) rides
    only frames a working companion in the same view already draws: with nothing at work the
    yard holds still. Two minutes after the person's last input on the machine (the app's own
    away rule, `slopty_platform::idle`), everyone in the yard but who waits on them or failed
    falls asleep. So lively costs nothing at rest and is the default, as the user asked for
    characters that run and play (MEASUREMENTS, "companions on the step clock").
  - **Reduce Motion: every companion holds its pose.** A working one breathes in opacity as
    the working mark does (the live cue the platform keeps), no moment plays, and a finished
    turn shows the raised arms for a second instead of a hop. **Increase Contrast** draws the
    outline cells in `text`, props and quiet marks in `text_secondary`. A companion has no role
    where a row says the state (the slot keeps the status's name); in the yard each is a
    button with a label. No sound, ever.
  - **Rejected.** Vendor mascots (Clawd is a registered design mark whose registration claims no
    colour; OpenAI and Google ask for no imitation); backdrops, confetti and scenes; a clock of
    their own for idle play (it would draw while nothing works); idle wandering in rows; drag
    to hand off; tamagotchi mechanics.
  - Tests: `companions::tests` (every kind has every pose on both grids, every pose looks its
    own, no two silhouettes share three quarters of their cells, the quads are few and are the
    frame, at least half of each agent companion's edge (a third of pi's coloured blocks)
    reads 3:1 on the content and panel planes in both variants and at both contrasts, Dot in
    the mark's own fixed green, an
    unknown agent keeps its accessory, the task from the call, the pose from the state, the
    working companion's twelve frames, the wave's six beats, no replay, Reduce Motion, off);
    `workspace::tests::companions` (no frame beyond the working mark's off, quiet or lively;
    the yard's order and click; the yard asleep while the person is away); `slopty-settings`
    `companions_values` and the schema's choice.

- ✅ **Each agent wears its owner's mark, in one colour** (2026-10-05,
  `.research/agent-marks-2026-10-05.md`, as ruled; supersedes "No agent wears a mark of its own"
  in `ui.md`). With the agent named only in words and every agent's row leading with one
  neutral glyph, the person could not find their Codex among their Claude Code threads at a
  glance. Zed, T3 Code, Vibe Kanban and the ACP registry all mark the agent with its owner's
  own mark.
  - **The marks.** Claude Code wears the Claude spark, Codex the `OpenAI` Blossom and pi its
    own ten cells on a 4 × 4 grid. Every other agent, an ACP agent among them, wears the neutral
    `text.bubble` and its name. A specific ACP agent gets a mark only once it proves a daily
    driver: 26 more owners' marks for agents rarely run would be clutter
    (`feedback-prune-critically`).
  - **The outlines are the owners' own, unmodified.** `crates/slopty-ui/assets/agents/`
    keeps `claude.svg` (the Agent Client Protocol registry's `claude-acp/icon.svg`, the same
    polygon as Anthropic's press kit file "Claude Spark - Clay.svg") and `openai.svg` (the
    registry's `codex-acp/icon.svg`), both Apache-2.0 as the registry ships them. pi has no
    file: its cells, as its MIT source and its one-colour favicon lay them, are a Rust
    constant (`icons::marks::PI_CELLS`). `LICENSE-acp-registry`, `LICENSE-pi` and `NOTICE`
    sit beside them; `NOTICE` names the trademarks' owners and says they are shown only to
    identify the agent a thread runs, with no endorsement or affiliation implied.
  - **One colour, never the brand's.** A mark is coverage alone, painted in the ink of the
    words beside it: no clay, no coral, so a Claude row is no louder than a Codex row and
    colour keeps meaning state. Light and dark need no variants.
  - **The rights, as found on 2026-10-05.** Anthropic's trademark guidelines ask for approval
    beforehand and forbid alterations to colour or proportion; its press kit publishes the
    spark and one-colour lockups. OpenAI's brand page forbids adding colours to the Blossom
    and using it as primary branding, and allows its logo only where it directly relates to
    OpenAI's services, which a Codex thread does. Neither grants a self-serve badge. The use
    is therefore tolerated rather than granted, as it is for the projects above; Slopty ships
    to internal TestFlight only. pi is MIT with no trademark statement.
  - **Guard rails.** The geometry exactly as published; one colour; only to say which agent a
    thread runs, never in Slopty's own icon, mark, site or store artwork; the name in words
    wherever an agent is chosen and in every tooltip and accessibility label. A drawing of our
    own that looked like either mark is ruled out: it would still be their mark, and an
    altered one.
  - **The switch back to words.** `icons::marks::WORDS_ONLY` lists the marks shown as words
    and the neutral glyph instead. Should an owner ask, its mark is added there in a one-line
    change. Asking for permission is the person's correspondence to send, not ours.
  - **Drawn by us at device pixels** (`slopty_platform::outline`). SVG path data is read
    (move, line, cubic, arc and close, absolute and relative), arcs become cubics and the ink
    box is worked out from the curves themselves. Core Graphics fills it with the non-zero
    rule into an alpha byte per pixel, the ink box's size, as the SF Symbols beside it are
    drawn; GPUI's own SVG path draws at twice the size and halves it, which cost the edges at
    1x. pi is filled cell by cell on whole pixels.
  - **Sized by the ink, centred on it.** A radial mark's ink box is its slot less the
    smallest space: 16 → 14 pt in a row's lead (the navigator's, the tile header's, the
    picker's), the one place a mark is drawn now. pi's square reads larger than a radial mark as wide, so it
    takes 0.86 of that on whole cells of at least two pixels (3 px cells, 12 px, in a row at
    1x). The mask is the ink box, so centring it centres the ink, and its origin is rounded to
    the device's grid. No weight: a filled silhouette does not thicken with a selected row's
    title.
  - **Measured** (1x, crisp = Σα²/Σα, solid = share of inked pixels at α ≥ 0.9;
    `icons::marks::tests::the_marks_are_crisp_at_1x`): the spark at 14 px 0.737 / 0.21, as
    crisp as the SF Symbols at 13 pt (0.731); the Blossom at 14 px 0.655 / 0.04, its inner
    strokes about 0.6 px; pi 1.0 / 1.0 (`docs/MEASUREMENTS.md`, "Agent marks at 1x").
  - Tests: `icons::marks::tests::{every_agent_wears_a_mark, a_mark_is_drawn_at_its_optical_size,
    a_mark_is_centred_on_its_ink, pi_lands_on_whole_pixels, the_marks_are_crisp_at_1x}`,
    `kit::tests::an_agents_mark_wears_no_colour_of_its_own`,
    `slopty-platform` `outline::tests::{path_data_reads_as_svg_reads_it,
    an_outline_lands_on_the_pixels_it_covers}`, and the `agent-marks` golden, light and dark
    (`slopty-e2e` `marks::each_agent_wears_its_mark_in_the_navigator`). Where the mark stands
    in a row is `ui.md`, "Identity leads, state trails".
