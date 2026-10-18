# phux developer commands.
# Mise or Nix gets the tools; these recipes are how you run the repo after that.
# `just` lists them. `just check` runs the CI lanes your diff needs.
#
# Recipes live in just/*.just so CI can route by the file that changed:
#   just/gates.just     lane recipes (guards, rust-lint, ...) and `check`
#   just/test.just      unit, e2e, stress
#   just/setup.just     doctor, toolchain, native smoke
#   just/build.just     developer builds
#   just/ffi.just       shared C ABI builds and checks
#   just/desktop.just   the GPUI desktop host
#   just/perf.just      local observability
#   just/release.just   packaging
#   just/mutation.just  optional scans
# Keep product recipes out of this file; the classifier treats it as cheap.

import 'just/test.just'
import 'just/setup.just'
import 'just/build.just'
import 'just/gates.just'
import 'just/ffi.just'
import 'just/desktop.just'
import 'just/web.just'
import 'just/perf.just'
import 'just/release.just'
import 'just/mutation.just'

default:
    @just --list
