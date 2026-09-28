# phux-server-testkit

Dev-only scaffolding for `phux-server`'s integration tests (and a few other
crates' wire tests): server spawn helpers, wire send/recv helpers, the
`E2eBuilder` multi-client harness, the libghostty-backed `Screen` oracle,
fault scripts, a relay harness, and tracing capture. It is a crate rather
than `tests/common/` so it compiles once instead of once per test binary.
