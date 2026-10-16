# Third-party notices

Slopty's interface draws on zeron's ideas and design. The code, fonts and icons Slopty takes
from the works below each keep their own licence.

## Zeron

Every file whose first line reads `// Derived from zeron (MIT, github.com/zeronsh/zeron @
<commit>)` is based on zeron at that commit; so are the other files of
`crates/slopty-shell`, `slopty-view-proto`, `slopty-view-doc`, `slopty-view-rpc`,
`slopty-shell-theme`, `slopty-syntax`, `slopty-markdown`, `slopty-veil`, `slopty-visual` and
`slopty-visual-gpu`.

```text
MIT License

Copyright (c) 2026 Wing

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## T3 Code's model manifest

`crates/slopty-ui/assets/t3code-model-manifest.json` is T3 Code's
`apps/server/src/provider/model-manifest.json` (github.com/pingdotgg/t3code, commit 08e18b78),
copyright 2026 T3 Tools Inc., under the MIT licence:

> Permission is hereby granted, free of charge, to any person obtaining a copy of this software
> and associated documentation files (the "Software"), to deal in the Software without
> restriction, including without limitation the rights to use, copy, modify, merge, publish,
> distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
> Software is furnished to do so, subject to the following conditions: The above copyright
> notice and this permission notice shall be included in all copies or substantial portions of
> the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
> PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
> LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
> OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
> DEALINGS IN THE SOFTWARE.

## Geist and Geist Mono

`crates/slopty-shell/assets/fonts/Geist*.ttf`, version 1.7.2. Copyright 2024 The Geist Project
Authors (github.com/vercel/geist-font). SIL Open Font License 1.1; the full text is
`crates/slopty-shell/assets/fonts/licenses/Geist-OFL.txt`.

## Solar Icons

The Solar glyphs in `crates/slopty-shell/assets/icons/`: "Solar Icons" by 480 Design (Linear weight), under Creative
Commons Attribution 4.0 International (https://creativecommons.org/licenses/by/4.0/). The glyphs
zeron drew in the same weight (terminal, stop, return, the expand arrows and their kin) come
under zeron's licence above.

## Symbols

`crates/slopty-shell/assets/file-icons/`: the Symbols file and folder icons, copyright 2020-22 Miguel Solorio, under the MIT licence in each
directory's `LICENSE.symbols`.
`files/serverless.svg` is renamed from upstream's misspelt file name.

## zui

The renderer features zeron's interface needs (backdrop blur, the rounded clip, the edge fade's
shape, the list's tail reservation, image eviction) are ported into gpui-fast from zui, zeron's
GPUI fork, under the Apache License 2.0 like GPUI itself; gpui-fast keeps the notice per port.

## gpui_tokio

`vendor/gpui_tokio` is Zed's bridge between GPUI and Tokio (github.com/zed-industries/zed,
`crates/gpui_tokio`), under the Apache License 2.0, whose text is in
`vendor/gpui_tokio/LICENSE-APACHE`.

## Vega, Vega-Lite, vega-embed, vega-interpreter and plotly.js

`crates/slopty-platform/assets/page-libs/` holds these libraries' npm builds byte for byte; a
page loads them from its own origin (`slopty_platform::web::page::libs`). Each licence's full
text is in `crates/slopty-platform/assets/page-libs/licenses/`.

| File | Package and version | Licence | Copyright |
| --- | --- | --- | --- |
| `vega.min.js` | vega 6.4.0 | BSD-3-Clause | 2015-2023 University of Washington Interactive Data Lab |
| `vega-lite.min.js` | vega-lite 6.5.0 | BSD-3-Clause | 2015 University of Washington Interactive Data Lab |
| `vega-embed.min.js` | vega-embed 7.3.0 | BSD-3-Clause | 2015 University of Washington Interactive Data Lab |
| `vega-interpreter.js` | vega-interpreter 2.3.2 | BSD-3-Clause | 2015-2023 University of Washington Interactive Data Lab |
| `plotly-cartesian.min.js` | plotly.js-cartesian-dist-min 4.1.2 | MIT | 2016-2024 Plotly Technologies Inc. |
