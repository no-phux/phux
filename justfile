# phux developer commands.
# Mise or Nix gets the tools; these recipes are how you run the repo after that.
# `just` (no args) lists the public API. CI-internal recipes are [private]
# and still invokable by name (`just fmt-check`, `just ci`, workflows).
#
# Recipes live in just/*.just so CI can route by the file that changed:
#   just/setup.just     native-setup (doctor, toolchain, smoke)
#   just/gates.just     root Rust gates (`just ci`)
#   just/test.just      unit/e2e/stress
#   just/build.just     developer builds
#   just/cockpit.just   cockpit-ci
#   just/perf.just      local observability (no product lane)
#   just/release.just   packaging (no product lane)
#   just/mutation.just  optional scans (no product lane)
# Keep product recipes out of this file; the classifier treats it as cheap.

import 'just/test.just'
import 'just/setup.just'
import 'just/build.just'
import 'just/gates.just'
import 'just/cockpit.just'
import 'just/desktop.just'
import 'just/perf.just'
import 'just/release.just'
import 'just/mutation.just'

default:
    @just --list
