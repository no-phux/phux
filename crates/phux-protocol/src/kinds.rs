//! The resource-kind catalog and the closed verb classification
//! ([ADR-0125], `docs/remote.md#who-can-do-what`, `docs/protocol.md#commands-events-resources`).
//!
//! One table serves discovery ([`SERVER_METHODS`], [`SUBSTRATE_METHODS`],
//! [`KINDS`]) and authorization ([`classify_frame`], [`classify_command`]):
//! method entries point at the same [`FRAME_RULES`] / [`COMMAND_RULES`] rows
//! the classifiers return, so the two cannot disagree. Compiled metadata,
//! never wire; reading it grants nothing.
//!
//! Both classifiers match exhaustively, so a new variant does not compile
//! until classified; anything the tables do not name is
//! [`Classification::Deny`].
//!
//! [ADR-0125]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0125-kind-catalog-is-generated-metadata.md

use crate::caps::{BootstrapCodec, ServerFeature, ServerFeatureExt};
use crate::ids::{ResourceId, ResourceKind, SatelliteHost};
// The catalog names every tag and key the codec declares.
#[allow(clippy::wildcard_imports)]
use crate::wire::frame::*;

include!("kinds/classification_types.rs");
include!("kinds/rules.rs");
include!("kinds/classifiers.rs");
include!("kinds/method_catalog.rs");
include!("kinds/resource_catalog.rs");
include!("kinds/contracts.rs");

/// Every catalog method: server-level, then substrate, then each kind's
/// facet in [`KINDS`] order.
pub fn methods() -> impl Iterator<Item = &'static MethodSpec> {
    SERVER_METHODS
        .iter()
        .chain(SUBSTRATE_METHODS)
        .chain(KINDS.iter().flat_map(|kind| kind.methods))
}

/// The catalog method carried by nested command `tag`, or `None` for a tag
/// this build does not allocate (which a decoder refuses and the classifier
/// would deny).
#[must_use]
pub fn command_method(tag: u8) -> Option<&'static MethodSpec> {
    methods().find(|method| method.carrier == Carrier::Command(tag))
}

/// The catalog method carried by client-to-server frame `type_byte`, or
/// `None` for a type that is unallocated, retired, or server-to-client.
#[must_use]
pub fn frame_method(type_byte: u8) -> Option<&'static MethodSpec> {
    methods().find(|method| method.carrier == Carrier::Frame(type_byte))
}

/// The catalog method named `name` (a wire name or a metadata key).
#[must_use]
pub fn method_named(name: &str) -> Option<&'static MethodSpec> {
    methods().find(|method| method.name == name)
}

/// The kind whose facet carries `command`, or `None` for a server or
/// substrate command. Read from the rows authorization uses, so routing and
/// classification cannot disagree.
#[must_use]
pub fn command_kind(command: &Command) -> Option<ResourceKind> {
    let rule = command_rule(command);
    KINDS
        .iter()
        .find(|kind| {
            kind.methods
                .iter()
                .any(|method| method.rules.iter().any(|row| core::ptr::eq(*row, rule)))
        })
        .map(|kind| kind.kind)
}

/// The kind whose app serves `command`: the kind whose facet carries it, or
/// the [`MethodSpec::served_by`] of the host-addressed method that does.
/// `None` for a command the host serves itself.
#[must_use]
pub fn command_served_by(command: &Command) -> Option<ResourceKind> {
    command_kind(command).or_else(|| {
        let rule = command_rule(command);
        SERVER_METHODS
            .iter()
            .find(|method| method.rules.iter().any(|row| core::ptr::eq(*row, rule)))
            .and_then(|method| method.served_by)
    })
}

/// Whether some dangerous method lists `rule` among its rows.
fn rule_is_dangerous(rule: &Rule) -> bool {
    methods()
        .filter(|method| method.dangerous)
        .any(|method| method.rules.iter().any(|row| core::ptr::eq(*row, rule)))
}

/// Whether this instance of a nested command is dangerous (ADR-0128): its
/// method is [`MethodSpec::dangerous`], except `SIGNAL_TERMINAL` with
/// `freeze` or `resume`, the reversible brake.
#[must_use]
pub fn command_is_dangerous(command: &Command) -> bool {
    let reversible = matches!(
        command,
        Command::SignalTerminal {
            signal: TerminalSignal::Freeze | TerminalSignal::Resume,
            ..
        }
    );
    !reversible && rule_is_dangerous(command_rule(command))
}

/// Whether this instance of a client frame is dangerous (ADR-0128): its
/// row belongs to a [`MethodSpec::dangerous`] method, except a decision to
/// `deny`, which releases nothing.
#[must_use]
pub fn frame_is_dangerous(frame: &FrameKind) -> bool {
    if let FrameKind::Command { command, .. } = frame {
        return command_is_dangerous(command);
    }
    let denial = matches!(frame, FrameKind::SetMetadata { value, .. } if value == APPROVAL_DENY);
    !denial && rule_is_dangerous(frame_rule(frame))
}

/// The catalog entry for `kind`, or `None` for a kind this build does not
/// serve.
#[must_use]
#[inline]
pub const fn kind_spec(kind: ResourceKind) -> Option<&'static KindSpec> {
    kind_spec_in(&KINDS, kind)
}

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod samples;
