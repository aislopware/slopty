# Third-party notices

Slopty's interface is copied from zeron. Its code, fonts and icons come from the works below,
each kept under its own licence.

## Zeron

Every file whose first line reads `// Derived from zeron (MIT, github.com/zeronsh/zeron @
053ce2e)` is copied from or adapted from zeron at that commit; so are the other files of
`crates/slopty-shell`, `slopty-view-proto`, `slopty-view-doc`, `slopty-view-rpc`,
`slopty-shell-theme`, `slopty-syntax`, `slopty-markdown` and `slopty-veil`.

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
