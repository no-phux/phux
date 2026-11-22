# Mutation testing

Regression tests belong in the ordinary suite; mutation runs are a bounded,
opt-in way to find tests that would not notice a change.

```sh
just mutation-rust-install                                     # pinned cargo-mutants
just mutation-rust                                             # default scope
just mutation-rust --file crates/phux-core/src/window.rs --limit 16
just mutation-rust --in-diff origin/main --list
```

A caught mutant is evidence for that test; a missed one is a place to look,
not necessarily a test to add. Unviable mutants and timeouts are not
evidence of anything.

See [Development](./development.md#validating-a-change) for the ordinary gates.
