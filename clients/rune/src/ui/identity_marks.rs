//! Bundled monochrome marks from @lobehub/icons (MIT).
//! See assets/identity/README.md and LICENSE. No external SVG is loaded.
use rune::Icon;

pub(super) static CLAUDE: Icon = Icon {
    name: "phux-identity-claude",
    view: 24.0,
    body: include_str!("../../assets/identity/claude.svg"),
};
pub(super) static CODEX: Icon = Icon {
    name: "phux-identity-codex",
    view: 24.0,
    body: include_str!("../../assets/identity/codex.svg"),
};
pub(super) static OPENCODE: Icon = Icon {
    name: "phux-identity-opencode",
    view: 24.0,
    body: include_str!("../../assets/identity/opencode.svg"),
};
