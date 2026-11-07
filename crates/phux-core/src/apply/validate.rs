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
        // A projection's parent supervises its producer, which runs in a
        // terminal (docs/resource-kinds.md).
        if matches!(
            spec.kind,
            ResourceKind::AgentSession
                | ResourceKind::AccessibilityTree
                | ResourceKind::PixelSurface
                | ResourceKind::Document
        ) && parent_kind != Some(ResourceKind::Terminal)
        {
            return Err(Refusal::invalid(
                "parent",
                "an agent conversation or a document resource binds to a terminal",
            ));
        }
        Ok(())
    }
}
