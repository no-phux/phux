//! What a new resource must satisfy before it is created.

use super::{LaunchMode, NewResource, Refusal, ResourceKind, Tx, check_name};

impl Tx<'_> {
    pub(super) fn validate_new_resource(&self, spec: &NewResource) -> Result<(), Refusal> {
        if let Some(name) = &spec.name {
            check_name("name", name)?;
        }
        match &spec.launch {
            Some(launch) if launch.kind() != spec.kind => {
                return Err(Refusal::invalid(
                    "launch",
                    "launch spec is for another kind",
                ));
            }
            None if spec.kind == ResourceKind::Terminal => {
                return Err(Refusal::invalid("launch", "a terminal needs a launch spec"));
            }
            None if spec.launch_mode == LaunchMode::Deferred => {
                return Err(Refusal::invalid(
                    "launch_mode",
                    "a deferred launch needs a launch spec",
                ));
            }
            _ => {}
        }
        let parent_kind = match spec.parent {
            Some(p) => Some(
                self.resource(p)
                    .ok_or_else(|| Refusal::invalid("parent", "unknown resource"))?
                    .kind,
            ),
            None => None,
        };
        // Only the structure is checked here: a kind that binds to a parent
        // has one that exists and can supervise a producer. Whether that
        // parent is fit is its app's check at spawn.
        if spec.kind.requires_parent() {
            match parent_kind {
                None => {
                    return Err(Refusal::invalid(
                        "parent",
                        format!("a {} binds to a parent", spec.kind),
                    ));
                }
                Some(kind) if !kind.can_parent() => {
                    return Err(Refusal::invalid(
                        "parent",
                        format!("a {kind} cannot supervise a producer"),
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}
