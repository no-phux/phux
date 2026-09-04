# Third-Party Notices

Phux Cockpit includes software from the following projects.

## Native SDK

Native SDK is licensed under the Apache License, Version 2.0. A complete copy
of that license is distributed as `LICENSE.txt` with the application.

Pinned source: https://github.com/phall1/native/tree/71bbce511187b9e071d3ca8cbf3b1b19138577da

Upstream: https://github.com/vercel-labs/native

## Ghostty and libghostty-vt

MIT License

Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors

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

Source: https://github.com/ghostty-org/ghostty

## Phux Client FFI and Rust Dependencies

Production builds statically link `phux-client-ffi` from the same source
checkout as Cockpit, through its stable C ABI. The package records the exact
Phux commit, workspace version, ABI version, and Cargo profile in
`Phux-FFI-Provenance.json`. Phux is available under MIT OR Apache-2.0.

The complete generated license inventory and license texts for the Rust
dependency graph are distributed beside this file as
`Phux-FFI-THIRD-PARTY.html`.

Source: https://github.com/no-phux/phux

## JetBrains Mono Nerd Font

Phux Cockpit embeds `JetBrainsMonoNL Nerd Font Mono` from Nerd Fonts v3.4.0 in
four weights — Regular, Bold, Italic, and BoldItalic — so that SGR bold and
italic select a real face rather than a synthesized one, and so that a bold
prompt keeps the same Nerd Font glyph coverage as a regular one. The patched
font is licensed under the SIL Open Font License, Version 1.1. The complete
copyright notice and license are distributed beside this file as
`JetBrainsMono-OFL.txt`.

JetBrains Mono copyright 2020 The JetBrains Mono Project Authors.
Nerd Fonts font patches copyright 2014 Ryan L McIntyre.

Source: https://github.com/ryanoasis/nerd-fonts/releases/tag/v3.4.0
