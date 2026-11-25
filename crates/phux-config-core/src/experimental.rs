//! `[experimental]`: flags that may change without a `SemVer` bump.

use serde::{Deserialize, Serialize};

/// `[experimental]`: opt-in flags that may change without a `SemVer` bump.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ExperimentalCfg {
    /// Mosh-class predictive local echo (ADR-0090). Unset lets the transport
    /// decide (see [`Self::predictive_echo_for`]); an explicit value wins.
    #[serde(rename = "predictive-echo")]
    pub predictive_echo: Option<bool>,
}

impl ExperimentalCfg {
    /// Resolve predictive echo for one attach: explicit config wins, else on
    /// only when the dial crosses a network. Local echo is fast enough that
    /// prediction could only cost its two undetectable flicker cases (vi
    /// mode at a readline prompt, no-echo password prompts).
    #[must_use]
    pub const fn predictive_echo_for(&self, remote_dial: bool) -> bool {
        match self.predictive_echo {
            Some(explicit) => explicit,
            None => remote_dial,
        }
    }
}
