//! `[organize]`: who judges where panes belong (ADR-0174).

use serde::{Deserialize, Serialize};

/// `[organize]` table: who judges where panes belong for `phux organize`
/// (ADR-0174). Read by the client that runs it, never by the server.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct OrganizeCfg {
    /// Argv whose trimmed stdout is the `TypeSafe` API key, run when
    /// `TYPESAFE_API_KEY` is unset. Unset means the environment only.
    pub jev_key_command: Option<Vec<String>>,
    /// Decide by repository matching only and send nothing off the machine.
    pub offline: bool,
}
