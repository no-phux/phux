# Third-party notices

phux is licensed under the Apache License, Version 2.0. Copyright 2026 phall.
See LICENSE and NOTICE.

This file is the canonical record of bundled third-party works that ship
in this tree or in a release tarball. Do not add a second LICENSE-* file
at the repository root for a bundled work; add a section here.

Crate licenses on crates.io are enforced by `deny.toml` (`just deny` /
`cargo deny`).

---

## neorex (MIT)

`plugins/nvim/src/windows.lua` is derived from neorex's `src/nvim/windows.lua`,
and the rest of `plugins/nvim` follows its design.
https://github.com/alexrlocke/neorex

MIT License

Copyright (c) 2026 Alex Locke

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

---

## Spleen bitmap font (BSD-2-Clause)

`crates/phux-record/assets/spleen-8x16.bdf` and the generated
`crates/phux-record/src/font/spleen_8x16.rs`, Spleen 2.1.0.

Copyright (c) 2018-2024, Frederic Cambus
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

  * Redistributions of source code must retain the above copyright
    notice, this list of conditions and the following disclaimer.

  * Redistributions in binary form must reproduce the above copyright
    notice, this list of conditions and the following disclaimer in the
    documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS
BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.

Source: https://www.cambus.net/spleen-monospaced-bitmap-fonts/

---

## phux-crash (Apache-2.0)

`crates/phux-crash` is a trimmed derivative of `xai-crash-handler` in xAI's
`grok-build` repository, keeping only the fatal-signal terminal restore.

    Copyright 2023-2026 SpaceXAI
    Licensed under the Apache License, Version 2.0

The crate's own LICENSE and NOTICE travel with it. Local modifications
are listed in that crate's Cargo.toml header.

---

## libghostty-vt (MIT)

Terminal emulation is consumed from the `libghostty-rs` git dependency
(see deny.toml allow-git). That project's license is MIT; the grant and
copyright remain with its authors. phux does not vendor the sources in
this tree.

---

## Cockpit

The archived Zig client retains its app-bundle notices (fonts, SDK, FFI
provenance) at `clients/cockpit/THIRD_PARTY_NOTICES.md` on tag
`archive/zig-cockpit-2026-10-14`. Those notices remain the surface for
historical Cockpit releases; that client is no longer shipped from main.
