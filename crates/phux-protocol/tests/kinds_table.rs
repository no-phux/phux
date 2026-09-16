//! The kind catalog against the spec it mirrors (ADR-0125).
//!
//! `phux_protocol::kinds` is both the discovery catalog and the source of
//! the workload-auth classifier, so it must agree with the normative text:
//! `docs/spec/workload-auth.md` §5 (verb bits) and §6 (the two
//! classification tables, compared row by row on case, requirement, and
//! subject), and `docs/spec/L1.md` §1.1 (each kind's facet). These tests
//! read the spec files and compare in both directions. A sweep over every
//! command tag uses the decoder as the oracle for which tags are allocated,
//! so the catalog cannot miss one or invent one. Which row each message
//! lands on is pinned per variant by the unit tests in `src/kinds/samples.rs`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions fail loudly with the remedy in the message"
)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use bytes::BytesMut;
use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::kinds::{
    self, COMMAND_RULES, Carrier, Classification, FRAME_RULES, Rule, Subject, Verb,
};
use phux_protocol::wire::error::DecodeError;
use phux_protocol::wire::frame::{Command, FrameKind};

/// How to fix a spec/catalog mismatch; there is nothing to regenerate.
const REMEDY: &str = "docs/spec/workload-auth.md is normative: edit the rows in \
     crates/phux-protocol/src/kinds.rs (case text verbatim, requirement and \
     subject equal) and the classifier arm that returns them, or amend the \
     spec first";

fn repo_doc(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()))
}

/// The text of `## N.` up to the next `## ` heading.
fn section<'a>(doc: &'a str, number: &str) -> &'a str {
    let heading = format!("\n## {number}. ");
    let start = doc
        .find(&heading)
        .unwrap_or_else(|| panic!("no section {number}"));
    let rest = &doc[start + 1..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |at| at + 3);
    &rest[..end]
}

/// Cells of a markdown table row; `None` for any other line or a separator.
fn table_cells(line: &str) -> Option<Vec<String>> {
    let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    if inner.trim_start().starts_with("---") {
        return None;
    }
    Some(
        inner
            .split('|')
            .map(|cell| cell.trim().to_owned())
            .collect(),
    )
}

/// One §6 row: case, requirement cell, subject cell.
struct SpecRow {
    case: String,
    requirement: String,
    subject: String,
}

/// §6's two tables: (frames, commands).
fn section_6_tables() -> (Vec<SpecRow>, Vec<SpecRow>) {
    let doc = repo_doc("docs/spec/workload-auth.md");
    let mut frames = Vec::new();
    let mut commands = Vec::new();
    let mut current: Option<&mut Vec<SpecRow>> = None;
    for line in section(&doc, "6").lines() {
        let Some(mut cells) = table_cells(line) else {
            continue;
        };
        match cells[0].as_str() {
            "Client-originated frame" => current = Some(&mut frames),
            "Command variant" => current = Some(&mut commands),
            _ => {
                let subject = cells.swap_remove(2);
                let requirement = cells.swap_remove(1);
                let case = cells.swap_remove(0);
                current
                    .as_deref_mut()
                    .expect("a §6 table row precedes its header")
                    .push(SpecRow {
                        case,
                        requirement,
                        subject,
                    });
            }
        }
    }
    (frames, commands)
}

/// A §6 requirement cell in the canonical form `Rule::requirement_label`
/// renders: verbs in bit order joined by `+`, or the exemption/deny word.
fn canonical_requirement(cell: &str) -> String {
    const WORDS: [(&str, &str); 6] = [
        ("handshake-exempt", "exempt: handshake"),
        ("self-exempt", "exempt: self"),
        ("liveness-exempt", "exempt: liveness"),
        ("cleanup-exempt", "exempt: cleanup"),
        ("default-deny", "deny"),
        ("classified by nested command tag", "nested"),
    ];
    if let Some((_, label)) = WORDS.iter().find(|(word, _)| cell.contains(word)) {
        return (*label).to_owned();
    }
    let mut label = Verb::ALL
        .iter()
        .filter(|verb| cell.contains(&format!("`{}`", verb.name())))
        .map(|verb| verb.name())
        .collect::<Vec<_>>()
        .join("+");
    assert!(
        !label.is_empty(),
        "unrecognised §6 requirement cell: {cell}"
    );
    if cell.contains("transport predicate") {
        label.push_str(" + owner-UDS transport");
    }
    label
}

const GLOBAL: Subject = Subject::Global {
    owner_uds_only: false,
};
const GLOBAL_OWNER_UDS: Subject = Subject::Global {
    owner_uds_only: true,
};

/// The §6 subject phrases and the `Subject` each one means. The first
/// prefix that matches wins, so a longer phrase precedes its own prefix.
const SPEC_SUBJECTS: [(&str, Subject); 24] = [
    ("named session", Subject::NamedSession),
    (
        "Global, and the authenticated transport MUST be the owner UDS",
        GLOBAL_OWNER_UDS,
    ),
    ("Global", GLOBAL),
    ("named Terminal", Subject::NamedTerminal),
    ("every named Terminal", Subject::EveryNamedTerminal),
    ("named satellite Host", Subject::SatelliteHost),
    (
        "every currently attached Terminal",
        Subject::AttachedTerminals,
    ),
    (
        "both moved and destination-owner Terminals",
        Subject::MovedAndOwnerTerminals,
    ),
    (
        "installs a filtered subscription over all observable Terminals",
        Subject::ObservableTerminals,
    ),
    (
        "requires at least one Inventory grant",
        Subject::InventoryMatches,
    ),
    ("resolved Group", Subject::ResolvedGroup),
    ("selected local Group", Subject::SelectedLocalGroup),
    ("payload Group", Subject::PayloadGroup),
    (
        "CREATE on the owner Terminal's",
        Subject::OwnerTerminalGroup,
    ),
    (
        "CREATE on the parent Terminal's",
        Subject::ParentTerminalGroup,
    ),
    (
        "CREATE on the satellite Host",
        Subject::SatelliteHostAndParent,
    ),
    (
        "the named resource's parent Terminal",
        Subject::ParentOfNamed,
    ),
    ("encoded metadata Scope", Subject::MetadataScope),
    ("the held action's subject", Subject::HeldAction),
    ("calling connection", Subject::CallingConnection),
    ("valid only in", Subject::None),
    ("no state access", Subject::None),
    ("nested subjects", Subject::None),
    ("none", Subject::None),
];

/// The `Subject` a §6 subject cell names. A denied row has no subject
/// whatever its cell explains.
fn spec_subject(requirement: &str, cell: &str) -> Subject {
    if requirement == "deny" {
        return Subject::None;
    }
    SPEC_SUBJECTS
        .iter()
        .find(|(phrase, _)| cell.starts_with(phrase))
        .map_or_else(
            || panic!("no Subject for §6 subject text {cell:?}; add its phrase to SPEC_SUBJECTS"),
            |(_, subject)| *subject,
        )
}

fn spec_lines(rows: &[SpecRow]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            let requirement = canonical_requirement(&row.requirement);
            let subject = spec_subject(&requirement, &row.subject);
            format!("| {} | {requirement} | {subject:?} |", row.case)
        })
        .collect()
}

fn catalog_lines(rules: &[&Rule]) -> Vec<String> {
    rules
        .iter()
        .map(|rule| {
            format!(
                "| {} | {} | {:?} |",
                rule.case,
                rule.requirement_label(),
                rule.subject
            )
        })
        .collect()
}

#[test]
fn catalog_markdown_matches_workload_auth_section_6() {
    let (frames, commands) = section_6_tables();
    assert_eq!(
        catalog_lines(&FRAME_RULES),
        spec_lines(&frames),
        "kinds::FRAME_RULES disagrees with the §6 client-frame table. {REMEDY}"
    );
    assert_eq!(
        catalog_lines(&COMMAND_RULES),
        spec_lines(&commands),
        "kinds::COMMAND_RULES disagrees with the §6 command table. {REMEDY}"
    );
}

#[test]
fn verbs_are_byte_equal_to_workload_auth_section_5() {
    let doc = repo_doc("docs/spec/workload-auth.md");
    // §5's first fenced block is the verb list; the next one is selectors.
    let verb_block = section(&doc, "5")
        .split("```")
        .nth(1)
        .expect("§5 opens with the verb block");
    let spec: Vec<(String, u8)> = verb_block.lines().filter_map(verb_line).collect();
    let catalog: Vec<(String, u8)> = Verb::ALL
        .iter()
        .map(|verb| (verb.name().to_owned(), verb.bit()))
        .collect();
    assert_eq!(catalog, spec, "Verb must mirror workload-auth.md §5");
}

/// `INVENTORY = 0x01   // comment` as `("INVENTORY", 0x01)`.
fn verb_line(line: &str) -> Option<(String, u8)> {
    let (name, rest) = line.split_once('=')?;
    let hex = rest.split_whitespace().next()?.strip_prefix("0x")?;
    Some((name.trim().to_owned(), u8::from_str_radix(hex, 16).ok()?))
}

/// The upper-case wire names in backticks in one cell.
fn backticked_wire_names(cell: &str) -> BTreeSet<String> {
    cell.split('`')
        .skip(1)
        .step_by(2)
        .filter(|token| {
            token.starts_with(|c: char| c.is_ascii_uppercase())
                && token
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
        .map(str::to_owned)
        .collect()
}

fn l1_facet(kind_name: &str) -> BTreeSet<String> {
    let doc = repo_doc("docs/spec/L1.md");
    let prefix = format!("| `{kind_name}` |");
    let line = doc
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("L1.md §1.1 has no facet row for {kind_name}"));
    let cells = table_cells(line).expect("the facet row is a table row");
    backticked_wire_names(&cells[2])
}

fn catalog_facet(kind: ResourceKind) -> BTreeSet<String> {
    kinds::kind_spec(kind)
        .expect("the kind is catalogued")
        .methods
        .iter()
        .map(|method| method.name.to_owned())
        .collect()
}

#[test]
fn terminal_facet_methods_match_l1_section_1_1_table() {
    assert_eq!(
        catalog_facet(ResourceKind::Terminal),
        l1_facet("TERMINAL"),
        "the TERMINAL facet in kinds.rs must equal docs/spec/L1.md §1.1"
    );
    assert_eq!(
        catalog_facet(ResourceKind::AgentSession),
        l1_facet("AGENT_SESSION"),
        "the AGENT_SESSION facet in kinds.rs must equal docs/spec/L1.md §1.1"
    );
}

/// Every `AgentEvent` in L1 §7.1's tag table, as `(tag, name)`.
fn l1_agent_events() -> BTreeSet<(u8, String)> {
    let doc = repo_doc("docs/spec/L1.md");
    let start = doc.find("\n### 7.1 ").expect("L1.md has §7.1");
    let rest = &doc[start..];
    let end = rest.find("\n### 7.2 ").expect("§7.1 ends at §7.2");
    rest[..end]
        .lines()
        .filter_map(table_cells)
        .filter_map(|cells| {
            let tag = u8::from_str_radix(cells[0].strip_prefix("0x")?, 16).ok()?;
            let name = cells[1].strip_prefix('`')?.strip_suffix('`')?;
            Some((tag, name.to_owned()))
        })
        .collect()
}

#[test]
fn every_agent_event_tag_is_catalogued_once() {
    let catalogued: Vec<(u8, String)> = kinds::SERVER_EVENTS
        .iter()
        .chain(kinds::SUBSTRATE_EVENTS)
        .chain(kinds::KINDS.iter().flat_map(|kind| kind.events))
        .map(|event| (event.tag, event.name.to_owned()))
        .collect();
    let unique: BTreeSet<(u8, String)> = catalogued.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        catalogued.len(),
        "an event is catalogued twice"
    );
    assert_eq!(
        unique,
        l1_agent_events(),
        "the catalog's events must equal docs/spec/L1.md §7.1's AgentEvent table"
    );
}

/// A complete `COMMAND` frame whose nested tag is `tag` and whose body is
/// otherwise empty.
fn command_frame_with_tag(tag: u8) -> Vec<u8> {
    let mut out = BytesMut::new();
    FrameKind::Command {
        request_id: 1,
        command: Command::Upgrade,
    }
    .encode(&mut out);
    let mut bytes = out.to_vec();
    *bytes.last_mut().expect("a frame has bytes") = tag;
    bytes
}

/// The decoder's answer to "is this command tag allocated?": it refuses an
/// unallocated tag as `UnknownEnumValue { field: "Command" }`, and anything
/// else (a decoded command, a truncated body) means the tag is known.
fn decoder_allocates_command_tag(tag: u8) -> bool {
    !matches!(
        FrameKind::decode(&command_frame_with_tag(tag)),
        Err(DecodeError::UnknownEnumValue {
            field: "Command",
            ..
        })
    )
}

/// The decoder's answer to "is this frame type allocated?".
fn decoder_allocates_frame_type(type_byte: u8) -> bool {
    !matches!(
        FrameKind::decode(&[0, 0, 0, 1, type_byte]),
        Err(DecodeError::UnknownFrameKind { .. })
    )
}

fn is_command_rule(rule: &Rule) -> bool {
    COMMAND_RULES.iter().any(|row| std::ptr::eq(*row, rule))
}

fn is_frame_rule(rule: &Rule) -> bool {
    FRAME_RULES.iter().any(|row| std::ptr::eq(*row, rule))
}

#[test]
fn every_command_tag_has_a_classification() {
    // The probe frame is sound: `UPGRADE` (0x0e) is bodyless, so the patched
    // byte is the nested tag.
    assert!(matches!(
        FrameKind::decode(&command_frame_with_tag(0x0e)),
        Ok((
            FrameKind::Command {
                command: Command::Upgrade,
                ..
            },
            _
        ))
    ));
    for tag in 0..=u8::MAX {
        let method = kinds::command_method(tag);
        assert_eq!(
            method.is_some(),
            decoder_allocates_command_tag(tag),
            "command tag {tag:#04x}: the catalog and the decoder disagree about \
             whether it is allocated; add or remove its MethodSpec in kinds.rs"
        );
        let Some(method) = method else {
            continue;
        };
        assert!(
            !method.rules.is_empty() && method.rules.iter().all(|rule| is_command_rule(rule)),
            "{} must name rows of COMMAND_RULES",
            method.name
        );
    }
}

#[test]
fn every_client_frame_type_has_a_classification() {
    for method in kinds::methods() {
        let Carrier::Frame(type_byte) = method.carrier else {
            continue;
        };
        assert_eq!(
            decoder_allocates_frame_type(type_byte),
            method.shipped,
            "{} ({type_byte:#04x}): a shipped frame must decode and a \
             spec-only one must not",
            method.name
        );
        assert!(
            method.rules.iter().all(|rule| is_frame_rule(rule)),
            "{} must name rows of FRAME_RULES",
            method.name
        );
    }
    // A server-to-client frame sent by a client is wrong-direction.
    assert_eq!(
        kinds::classify_frame(&FrameKind::Pong { nonce: 1 }),
        Classification::Deny
    );
    assert!(kinds::frame_method(FrameKind::Pong { nonce: 1 }.type_byte()).is_none());
    let local = ResourceId::local(1);
    assert!(
        kinds::frame_method(
            FrameKind::ResizeTerminal {
                terminal_id: local,
                cols: 80,
                rows: 24,
            }
            .type_byte()
        )
        .is_some()
    );
}

#[test]
fn unknown_and_retired_tags_classify_as_deny() {
    // SPAWN 0x00, RESIZE_TERMINAL 0x04, RUN_HOOK 0x06 (unallocated); 0x0a and
    // 0x0b (freed by the dissolved session verbs).
    for tag in [0x00, 0x04, 0x06, 0x0a, 0x0b] {
        assert!(kinds::command_method(tag).is_none(), "{tag:#04x}");
        assert!(!decoder_allocates_command_tag(tag), "{tag:#04x}");
    }
    // SUBSCRIBE (0x40) is unallocated; INPUT_RAW (0x13) is spec-only.
    assert!(kinds::frame_method(0x40).is_none());
    assert!(!decoder_allocates_frame_type(0x40));
    for case in [
        "`SPAWN` (unallocated)",
        "`RESIZE_TERMINAL` (unallocated)",
        "`RUN_HOOK` (unallocated)",
        "Unknown, retired, or otherwise unclassified command tag",
    ] {
        let rule = COMMAND_RULES
            .iter()
            .find(|rule| rule.case == case)
            .unwrap_or_else(|| panic!("no command row {case}"));
        assert_eq!(rule.classification(), Classification::Deny, "{case}");
    }
    for case in [
        "`SUBSCRIBE` (unallocated)",
        "Unknown, wrong-direction, retired, or otherwise unclassified frame",
    ] {
        let rule = FRAME_RULES
            .iter()
            .find(|rule| rule.case == case)
            .unwrap_or_else(|| panic!("no frame row {case}"));
        assert_eq!(rule.classification(), Classification::Deny, "{case}");
    }
}

/// Rows for traffic no method carries: unallocated frames and tags, and the
/// command default-deny row. Every other row must be reachable from some
/// catalog method; the frame default-deny row is, because a spawn with an
/// unclassified binding lands on it.
const UNCARRIED_ROWS: [&str; 5] = [
    "`SUBSCRIBE` (unallocated)",
    "`SPAWN` (unallocated)",
    "`RESIZE_TERMINAL` (unallocated)",
    "`RUN_HOOK` (unallocated)",
    "Unknown, retired, or otherwise unclassified command tag",
];

#[test]
fn catalog_rows_and_methods_are_consistent() {
    let methods: Vec<_> = kinds::methods().collect();
    let names: BTreeSet<_> = methods.iter().map(|method| method.name).collect();
    assert_eq!(names.len(), methods.len(), "method names are unique");
    for (i, method) in methods.iter().enumerate() {
        assert!(
            methods[..i]
                .iter()
                .all(|other| other.carrier != method.carrier),
            "{} shares a carrier with another method",
            method.name
        );
        let in_table = match method.carrier {
            Carrier::Command(_) => is_command_rule,
            Carrier::Frame(_) | Carrier::Metadata(_) => is_frame_rule,
        };
        assert!(
            method.rules.iter().all(|rule| in_table(rule)),
            "{} names a row outside its table",
            method.name
        );
    }
    for rule in FRAME_RULES.iter().chain(COMMAND_RULES.iter()) {
        let carried = methods
            .iter()
            .any(|method| method.rules.iter().any(|row| std::ptr::eq(*row, *rule)));
        assert_eq!(
            carried,
            !UNCARRIED_ROWS.contains(&rule.case),
            "row {} is carried by a method iff it is not an unallocated row",
            rule.case
        );
    }
    for kind in &kinds::KINDS {
        assert_eq!(
            kinds::kind_spec(kind.kind).map(|spec| spec.name),
            Some(kind.name)
        );
    }
}
