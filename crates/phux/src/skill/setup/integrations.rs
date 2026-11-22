//! Which detected agents have the phux integration that reports their state
//! (`docs/agents.md#agent-setup`), for `phux doctor`'s checklist.

use super::{ALL, Ctx, SkillAgent};

/// One detected agent and its status integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Integration {
    /// The agent's display name.
    pub(crate) agent: &'static str,
    /// Whether the integration is installed; `None` when phux has none for
    /// this agent, so its state comes from the screen.
    pub(crate) installed: Option<bool>,
    /// The command that installs it.
    pub(crate) install: &'static str,
}

/// Every agent installed on this host, with its integration.
pub(crate) fn agent_integrations() -> Result<Vec<Integration>, String> {
    let ctx = Ctx::from_env(false)?;
    Ok(ALL
        .into_iter()
        .filter(|agent| agent.detected(&ctx))
        .map(|agent| integration(agent, &ctx))
        .collect())
}

fn integration(agent: SkillAgent, ctx: &Ctx) -> Integration {
    // ponytail: a substring match, since OpenCode's config may be JSONC; a
    // commented-out plugin line reads as installed.
    let mentions = |files: &[&str], package: &str| {
        files.iter().any(|file| {
            std::fs::read_to_string(ctx.home.join(file)).is_ok_and(|body| body.contains(package))
        })
    };
    match agent {
        SkillAgent::Claude => Integration {
            agent: "Claude Code",
            installed: Some(super::hook::hook_owner(ctx).is_some()),
            install: "claude plugin marketplace add no-phux/phux && claude plugin install phux@phux",
        },
        SkillAgent::Opencode => Integration {
            agent: "OpenCode",
            installed: Some(mentions(
                &[
                    ".config/opencode/opencode.json",
                    ".config/opencode/opencode.jsonc",
                ],
                "@phux/opencode",
            )),
            install: "opencode plugin add @phux/opencode",
        },
        SkillAgent::Pi => Integration {
            agent: "pi",
            installed: Some(mentions(&[".pi/agent/settings.json"], "@phux/pi")),
            install: "pi install npm:@phux/pi",
        },
        SkillAgent::Codex => Integration {
            agent: "Codex",
            installed: None,
            install: "",
        },
    }
}
