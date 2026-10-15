# Shared unwind-safe C ABI archives for native embedders.

[group('build')]
ffi-build:
    cargo rustc --locked --profile ffi-dev -p phux-client-ffi --lib --crate-type staticlib


[group('test')]
[doc('Compile, link, and run the C ABI checks against the same-checkout archive.')]
ffi-abi: ffi-build
    bash crates/phux-client-ffi/tests/run_abi_checks.sh ffi-dev


[group('build')]
ffi-release:
    cargo rustc --locked --profile ffi-release -p phux-client-ffi --lib --crate-type staticlib
