//! The read-only and destructive hints a row's [`Touches`] imply, derived
//! from the kind table rather than written by hand.

use phux_protocol::ids::ResourceId;
use phux_protocol::kinds::{self, Verb, Verbs};
use phux_protocol::wire::frame::{Command, FrameKind, RESOURCE_TAGS_KEY, Scope};

use super::{METADATA_WRITE, PERF_READ, Touches, row};

/// The two hints one tool carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hints {
    /// No method the tool can send changes server state.
    pub(crate) read_only: bool,
    /// Some method can end a process, eject a client, or type into a PTY.
    pub(crate) destructive: bool,
}

/// What an unmapped tool is annotated as: the most conservative claim.
pub(crate) const UNMAPPED: Hints = Hints {
    read_only: false,
    destructive: true,
};

/// Where the `destructive` column comes from.
pub(crate) const DESTRUCTIVE_SOURCE: &str = "the kind table's `MethodSpec.dangerous` flag, \
     or a method that needs `INPUT`";

/// Whether a method is destructive: the kind table marks it `dangerous`
/// (it can end a process, eject a client, stop the server, or release a
/// held action; ADR-0128), or it types into a live PTY (`INPUT`). `CREATE`
/// and `BIND` add resources and rewrite bindings: mutating, not destructive.
const fn dangerous(verbs: Verbs, marked: bool) -> bool {
    marked || verbs.contains(Verb::Input)
}

/// The hints `touches` implies, or `None` when it names a method the
/// catalog does not have.
pub(crate) fn hints(touches: Touches) -> Option<Hints> {
    match touches {
        Touches::LocalExec => Some(UNMAPPED),
        Touches::LocalRead => Some(Hints {
            read_only: true,
            destructive: false,
        }),
        Touches::LocalWrite => Some(Hints {
            read_only: false,
            destructive: false,
        }),
        Touches::Wire(names) => wire_hints(names),
    }
}

fn wire_hints(names: &[&str]) -> Option<Hints> {
    let mut hints = Hints {
        read_only: true,
        destructive: false,
    };
    for name in names {
        let (verbs, mutating, marked) = method_facts(name)?;
        hints.read_only &= !mutating;
        hints.destructive |= dangerous(verbs, marked);
    }
    Some(hints)
}

/// The verbs `name` can need, whether it can change state
/// ([`kinds::MethodSpec::mutating`]'s conservative rule), and whether the
/// kind table marks it dangerous.
fn method_facts(name: &str) -> Option<(Verbs, bool, bool)> {
    if name == METADATA_WRITE {
        let verbs = kinds::frame_rule(&ordinary_metadata_write()).verb_set();
        // A row that admits no verb is a denial: conservatively a write.
        return Some((verbs, verbs.is_empty() || verbs.mutates(), false));
    }
    if name == PERF_READ {
        let verbs = kinds::command_rule(&Command::GetPerf { reset: false }).verb_set();
        return Some((verbs, verbs.is_empty() || verbs.mutates(), false));
    }
    let method = kinds::method_named(name)?;
    Some((method.verbs(), method.mutating(), method.dangerous))
}

/// A representative ordinary write: a Terminal's tags.
fn ordinary_metadata_write() -> FrameKind {
    FrameKind::SetMetadata {
        request_id: 0,
        scope: Scope::Resource(ResourceId::local(1)),
        key: RESOURCE_TAGS_KEY.to_owned(),
        value: Vec::new(),
    }
}

/// The hints for `tool`, or `None` with no row or an unknown method.
pub(crate) fn hints_for(tool: &str) -> Option<Hints> {
    row(tool).and_then(|row| hints(row.touches))
}
