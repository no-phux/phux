# Third-party notices

phux is licensed under the Apache License, Version 2.0. Copyright 2026 phall.
See LICENSE and NOTICE.

This file is the canonical record of bundled third-party works that ship
in this tree or in a release tarball. Do not add a second LICENSE-* file
at the repository root for a bundled work; add a section here.

Crate licenses on crates.io are enforced by `deny.toml` (`just deny` /
`cargo deny`).

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

`crates/phux-crash` is vendored from `xai-crash-handler` in xAI's
`grok-build` repository.

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

The native macOS client at clients/cockpit/ ships its own
THIRD_PARTY_NOTICES.md for the app bundle (fonts, SDK, FFI provenance).
That file is the notices surface for Cockpit releases.
