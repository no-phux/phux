//! `[experimental]`: the table `[labs]` replaced; still read.

use serde::{Deserialize, Serialize};

/// `[experimental]`: superseded by `[labs]` ([`crate::labs`]). Its one key
/// is still honored when `[labs]` leaves the same lab unset.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ExperimentalCfg {
    /// Deprecated: use `[labs] predictive-echo`, which wins when both are set.
    #[serde(rename = "predictive-echo")]
    pub predictive_echo: Option<bool>,
}
