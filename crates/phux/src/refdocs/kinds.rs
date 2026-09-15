//! The generated kind-catalog reference, rendered from
//! `phux_protocol::kinds`: the same table the workload-auth classifier reads
//! (ADR-0125), so this page, `phux --capabilities --json`, and dispatch
//! cannot disagree about what a method needs.

use std::fmt::Write as _;

use phux_protocol::caps::ServerFeature;
use phux_protocol::kinds::{
    self, COMMAND_RULES, Carrier, EventSpec, FRAME_RULES, KindSpec, MethodSpec, Rule, Verb,
};

use super::Page;

/// Render `docs/reference/kinds.md`.
pub(crate) fn page() -> Page {
    let mut body = String::from(
        "Everything below is compiled into the binary from \
         `phux_protocol::kinds`. The same rows drive `phux --capabilities \
         --json` and the workload-auth classifier \
         (`docs/spec/workload-auth.md` §6), so the verbs listed for a method \
         are the verbs dispatch requires. A method is invoked through its \
         typed frame or command. The catalog describes methods and is never \
         an invocation handle, and reading it grants nothing.\n\n\
         Verbs are the closed set of workload-auth §5. A method whose rows \
         need only `INVENTORY` or `OBSERVE` is read-only; any other verb can \
         change server state; a method no row admits by verb (the `COMMAND` \
         envelope, a denied method) counts as mutating. A gate is the \
         `HELLO_OK` feature bit a client must see before relying on the \
         method, named as `phux status --json` names it under `features`. \
         In `phux --capabilities --json` a gate is `{ \"feature\": name, \
         \"mask\": value }`, where `mask` is the bit's value in \
         `HELLO_OK.server_caps.features`, not a bit index.\n",
    );
    push_methods(
        &mut body,
        "## Server methods",
        "Addressed to the server or the connection rather than to one resource.",
        kinds::SERVER_METHODS,
    );
    push_events(&mut body, "## Server events", kinds::SERVER_EVENTS);
    push_methods(
        &mut body,
        "## Substrate methods",
        "Answered by every resource kind.",
        kinds::SUBSTRATE_METHODS,
    );
    push_events(&mut body, "## Substrate events", kinds::SUBSTRATE_EVENTS);
    for kind in &kinds::KINDS {
        push_kind(&mut body, kind);
    }
    push_rules(
        &mut body,
        "## Client frame classification",
        "Every client-originated frame lands on exactly one row.",
        &FRAME_RULES,
    );
    push_rules(
        &mut body,
        "## Command classification",
        "Every command nested in `COMMAND` lands on exactly one row; the \
         envelope alone grants nothing.",
        &COMMAND_RULES,
    );

    Page {
        file: "kinds.md",
        title: "phux kind catalog reference",
        summary: "Every resource kind, its methods and events, and the verb each frame and command needs.",
        tldr: "The compiled kind catalog: server-level, substrate, and \
               per-kind methods, the events each kind emits, and the \
               workload-auth verb classification of every client frame and \
               command. Rendered from `phux_protocol::kinds`, the table the \
               classifier reads, so the page cannot drift from dispatch.",
        body,
    }
}

fn push_kind(body: &mut String, kind: &KindSpec) {
    let _ = write!(
        body,
        "\n## Kind `{}`\n\nWire tag `{}`; gate: {}.\n",
        kind.name,
        kind.kind.as_wire(),
        gate(kind.gate),
    );
    push_methods(body, "### Methods", "The kind's facet.", kind.methods);
    push_events(body, "### Events", kind.events);
    body.push_str("\n### Resource metadata keys\n\n");
    if kind.metadata_keys.is_empty() {
        body.push_str("None.\n");
    }
    for key in kind.metadata_keys {
        let _ = writeln!(body, "- `{key}`");
    }
}

fn push_methods(body: &mut String, heading: &str, intro: &str, methods: &[MethodSpec]) {
    let _ = write!(
        body,
        "\n{heading}\n\n{intro}\n\n\
         | Method | Carrier | Requires | Mutating | Gate | Status |\n\
         |---|---|---|---|---|---|\n"
    );
    for method in methods {
        let _ = writeln!(
            body,
            "| `{}` | {} | {} | {} | {} | {} |",
            method.name,
            carrier(method.carrier),
            requires(method),
            if method.mutating() { "yes" } else { "no" },
            gate(method.gate),
            if method.shipped {
                "shipped"
            } else {
                "spec-only"
            },
        );
    }
}

fn push_events(body: &mut String, heading: &str, events: &[EventSpec]) {
    let _ = write!(body, "\n{heading}\n\n");
    if events.is_empty() {
        body.push_str("None.\n");
        return;
    }
    body.push_str("| Event | Tag |\n|---|---|\n");
    for event in events {
        let _ = writeln!(body, "| `{}` | `0x{:02x}` |", event.name, event.tag);
    }
}

fn push_rules(body: &mut String, heading: &str, intro: &str, rules: &[&Rule]) {
    let _ = write!(
        body,
        "\n{heading}\n\n{intro}\n\n| Case | Requires | Subject |\n|---|---|---|\n"
    );
    for rule in rules {
        let _ = writeln!(
            body,
            "| {} | {} | `{:?}` |",
            rule.case,
            rule.requirement_label(),
            rule.subject,
        );
    }
}

fn carrier(carrier: Carrier) -> String {
    match carrier {
        Carrier::Frame(type_byte) => format!("frame `0x{type_byte:02x}`"),
        Carrier::Command(tag) => format!("command `0x{tag:02x}`"),
        Carrier::Metadata(_) => "metadata key".to_owned(),
    }
}

/// The verbs a method can need, or, for a method no row grants by verb, the
/// distinct labels of its rows (`exempt: ...`, `nested`, `deny`).
fn requires(method: &MethodSpec) -> String {
    let verbs = method.verbs();
    if verbs.is_empty() {
        let mut labels: Vec<String> = method
            .rules
            .iter()
            .map(|rule| rule.requirement_label())
            .collect();
        labels.dedup();
        return labels.join("; ");
    }
    let mut label = verbs.iter().map(Verb::name).collect::<Vec<_>>().join(", ");
    if method.owner_uds_only() {
        label.push_str(" (owner socket only)");
    }
    label
}

fn gate(gate: Option<ServerFeature>) -> String {
    let name = gate.and_then(crate::feature_names::feature_name);
    name.map_or_else(|| "none".to_owned(), |name| format!("`{name}`"))
}

#[cfg(test)]
mod tests {
    use phux_protocol::kinds::{self, COMMAND_RULES, FRAME_RULES};

    use super::page;

    /// Every catalog method and every classification row appears on the page.
    #[test]
    fn kinds_page_lists_every_method_and_rule() {
        let body = page().body;
        for method in kinds::methods() {
            assert!(
                body.contains(&format!("| `{}` |", method.name)),
                "kinds.md has no row for method {}",
                method.name
            );
        }
        for rule in FRAME_RULES.iter().chain(COMMAND_RULES.iter()) {
            assert!(
                body.contains(&format!("| {} | {} |", rule.case, rule.requirement_label())),
                "kinds.md has no row for rule {}",
                rule.case
            );
        }
    }
}
