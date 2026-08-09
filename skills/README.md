# skills/

`phux/SKILL.md` is the agent skill `phux skill` prints. It is **compiled into
the binary** with `include_str!` (see `crates/phux/src/main.rs`), so the text an
agent reads always belongs to the binary it is driving. There is no separate
shipped copy to fall out of date.

Editing it is editing the CLI's agent-facing contract. Three tests in
`crates/phux/src/help_inventory.rs` hold it to the clap tree:

- every visible top-level verb is mentioned,
- every visible `phux agent` subcommand is mentioned,
- every selector sigil the parser accepts is taught.

So adding a verb or a sigil without documenting it fails `just test`, with the
file to edit named in the failure message. That is the whole point: the skill
is a build artifact of the surface, not a hope.

The example skills under `examples/skills/` are separate, hand-maintained
illustrations (CLI + MCP orchestration, the `phux-terminal` read/act loop).
They are examples, not the contract; when they disagree with `phux skill`,
`phux skill` wins.
