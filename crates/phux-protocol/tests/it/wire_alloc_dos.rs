//! Repro + regression: attacker-controlled count prefixes must not drive
//! pre-allocation disproportionate to the bytes actually present in the frame.
//!
//! A tiny frame (single-digit bytes) that declares a 4-billion-element list
//! pre-`fix` calls `Vec::with_capacity(4e9)`. Depending on the platform that
//! either aborts the process (allocator returns null -> `handle_alloc_error`)
//! or silently reserves tens of GiB of address space. Either way it is a
//! decode-path denial of service: the decoder must reject the frame having
//! reserved no more
//! than the remaining input could justify.
//!
//! The binary's global allocator (`crate::alloc_probe`) captures the single
//! largest allocation request made on this thread while decoding, so the
//! assertion is deterministic across platforms (it does not depend on whether
//! the OS overcommits).

#![allow(clippy::unwrap_used)]

use std::cell::Cell;

use phux_protocol::caps::BootstrapLimits;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    FrameKind, MAX_APPEND_BYTES, MAX_HISTORY_CURSOR_BYTES, MAX_HISTORY_PAGE_ROWS,
    MAX_INPUT_TERMINAL_REPLY_BYTES, TYPE_BOOTSTRAP_CHUNK, TYPE_COMMAND, TYPE_HISTORY_PAGE,
    TYPE_HISTORY_REJECTED, TYPE_HISTORY_TOMBSTONE, TYPE_INPUT_TERMINAL_REPLY,
};

use crate::common::{framed_tlv, put_varint, tlv_field};

fn framed(body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

std::thread_local! {
    static MAX_ALLOC: Cell<usize> = const { Cell::new(0) };
    static RECORDING: Cell<bool> = const { Cell::new(false) };
}

/// Record an allocation request of `size` bytes while a decode is being
/// measured. The binary's global allocator (`crate::alloc_probe`) calls this.
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub` alone trips `unreachable_pub`; the allocator in `main.rs` calls this"
)]
pub(crate) fn record(size: usize) {
    if RECORDING.try_with(Cell::get).unwrap_or(false) {
        let _ = MAX_ALLOC.try_with(|max| max.set(max.get().max(size)));
    }
}

fn largest_alloc_during(decode_input: &[u8]) -> usize {
    MAX_ALLOC.set(0);
    RECORDING.set(true);
    let _ = FrameKind::decode(decode_input);
    RECORDING.set(false);
    MAX_ALLOC.get()
}

fn largest_alloc_during_with_limits(
    decode_input: &[u8],
    limits: BootstrapLimits,
) -> (Result<(), DecodeError>, usize) {
    MAX_ALLOC.set(0);
    RECORDING.set(true);
    let result = FrameKind::decode_with_limits(decode_input, limits).map(|_| ());
    RECORDING.set(false);
    (result, MAX_ALLOC.get())
}

/// A decoder that trusts a declared count reserves gigabytes for a frame that
/// carries none of the promised elements. 1 MiB is a generous ceiling for
/// anything these fixtures legitimately need.
const ALLOC_CEILING: usize = 1 << 20;

/// Assert a hostile frame both fails to decode and never reserves on the order
/// of its declared count rather than its actual size.
///
/// The four cases below differ only in which field carries the bogus
/// `u32::MAX`; before this they also differed in what their failure message
/// bothered to report, which is not a difference worth keeping.
fn assert_bounded_alloc(frame: &[u8], what: &str) {
    let max = largest_alloc_during(frame);
    assert!(
        FrameKind::decode(frame).is_err(),
        "{what}: a frame declaring u32::MAX elements must not decode"
    );
    assert!(
        max < ALLOC_CEILING,
        "{what}: decoder reserved {max} bytes for a {}-byte frame",
        frame.len()
    );
}

#[test]
fn metadata_keys_huge_count_does_not_over_reserve() {
    // METADATA_KEYS (0xD2): the KEYS field (id 2) value is a positional u32
    // count + strings. Declare count = u32::MAX inside a tiny field.
    let mut keys_value = Vec::new();
    keys_value.extend_from_slice(&u32::MAX.to_be_bytes());
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u32.to_be_bytes()); // request_id
    tlv_field(&mut fields, 2, &keys_value); // keys: huge count, no elements
    let frame = framed_tlv(0xD2, &fields);
    assert_bounded_alloc(&frame, "metadata keys");
}

#[test]
fn directory_listing_huge_entry_count_does_not_over_reserve() {
    // DIRECTORY_LISTING (0xD3): the ENTRIES field (id 4) value is a
    // positional u32 count + (name, flags) pairs. Declare count = u32::MAX
    // inside a tiny field.
    let mut entries_value = Vec::new();
    entries_value.extend_from_slice(&u32::MAX.to_be_bytes());
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u32.to_be_bytes()); // request_id
    tlv_field(&mut fields, 2, b"/"); // path
    tlv_field(&mut fields, 4, &entries_value); // entries: huge count
    let frame = framed_tlv(0xD3, &fields);
    assert_bounded_alloc(&frame, "directory listing entries");
}

#[test]
fn spawn_terminal_huge_command_list_does_not_over_reserve() {
    // SPAWN_RESOURCE (0x22): the COMMAND field (id 3) value is a positional u32
    // count + strings. Declare count = u32::MAX inside a tiny field.
    let mut cmd_value = Vec::new();
    cmd_value.extend_from_slice(&u32::MAX.to_be_bytes());
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u32.to_be_bytes()); // request_id
    tlv_field(&mut fields, 2, &1u32.to_be_bytes()); // group
    tlv_field(&mut fields, 3, &cmd_value); // command: huge count
    let frame = framed_tlv(0x22, &fields);
    assert_bounded_alloc(&frame, "spawn command list");
}

#[test]
fn spawn_terminal_huge_env_list_does_not_over_reserve() {
    // SPAWN_RESOURCE env: the ENV field (id 5) value is a positional u32 count
    // + pairs. Declare count = u32::MAX inside a tiny field.
    let mut env_value = Vec::new();
    env_value.extend_from_slice(&u32::MAX.to_be_bytes());
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u32.to_be_bytes()); // request_id
    tlv_field(&mut fields, 2, &1u32.to_be_bytes()); // group
    tlv_field(&mut fields, 5, &env_value); // env: huge count
    let frame = framed_tlv(0x22, &fields);
    assert_bounded_alloc(&frame, "spawn env list");
}

#[test]
fn attached_snapshot_huge_sessions_list_does_not_over_reserve() {
    // ATTACHED (0x81): the SNAPSHOT field (id 1) value is a positional
    // SessionSnapshot that starts with a u32 sessions count. Declare
    // count = u32::MAX inside a tiny field.
    let mut snap_value = Vec::new();
    snap_value.extend_from_slice(&u32::MAX.to_be_bytes()); // sessions count
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap_value); // snapshot
    let frame = framed_tlv(0x81, &fields);
    assert_bounded_alloc(&frame, "attached sessions list");
}

#[test]
fn negotiated_oversize_bootstrap_chunk_rejects_before_payload_allocation() {
    let limits = BootstrapLimits::new(1024, 2048).unwrap();
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    let payload = vec![0xA5; 256 * 1024];
    let mut body = vec![TYPE_BOOTSTRAP_CHUNK];
    tlv_field(&mut body, 1, &terminal);
    tlv_field(&mut body, 2, &1_u64.to_be_bytes());
    tlv_field(&mut body, 3, &1_u64.to_be_bytes());
    tlv_field(&mut body, 4, &0_u32.to_be_bytes());
    tlv_field(&mut body, 5, &payload);
    let frame = framed(&body);

    let (result, max) = largest_alloc_during_with_limits(&frame, limits);
    assert_eq!(result.unwrap_err(), DecodeError::BootstrapLimitExceeded);
    assert_eq!(
        max, 0,
        "oversized borrowed payload must fail before allocation"
    );
}

#[test]
fn malformed_bootstrap_payload_length_rejects_without_reserving() {
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    let mut body = vec![TYPE_BOOTSTRAP_CHUNK];
    tlv_field(&mut body, 1, &terminal);
    tlv_field(&mut body, 2, &1_u64.to_be_bytes());
    tlv_field(&mut body, 3, &1_u64.to_be_bytes());
    tlv_field(&mut body, 4, &0_u32.to_be_bytes());
    put_varint(&mut body, 5);
    body.push(4);
    put_varint(&mut body, u64::MAX);
    let frame = framed(&body);

    let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
    assert!(matches!(
        result,
        Err(DecodeError::LengthOverflow | DecodeError::UnexpectedEof)
    ));
    assert_eq!(
        max, 0,
        "malformed payload length must fail before allocation"
    );
}

#[test]
fn oversized_terminal_reply_rejects_before_owned_bytes_allocation() {
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    let payload = vec![0xA5; MAX_INPUT_TERMINAL_REPLY_BYTES + 1];
    let mut body = vec![TYPE_INPUT_TERMINAL_REPLY];
    tlv_field(&mut body, 1, &terminal);
    tlv_field(&mut body, 2, &payload);
    let frame = framed(&body);

    let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
    assert_eq!(
        result.unwrap_err(),
        DecodeError::InputTerminalReplyLimitExceeded
    );
    assert_eq!(
        max, 0,
        "oversized terminal reply must fail before Bytes allocation"
    );
}

#[test]
fn oversized_append_resource_output_rejects_before_owned_bytes_allocation() {
    // COMMAND (0x31) carrying APPEND_RESOURCE_OUTPUT (tag 0x1a): the payload
    // is a u32-prefixed leaf `bytes` inside the positional command value. One
    // byte over the cap must be refused before the Vec copy is made.
    let mut command = vec![0x1au8];
    command.push(0); // ResourceId::Local tag
    command.extend_from_slice(&1_u32.to_be_bytes());
    let payload_len = MAX_APPEND_BYTES + 1;
    command.extend_from_slice(&u32::try_from(payload_len).unwrap().to_be_bytes());
    command.extend(std::iter::repeat_n(0xA5u8, payload_len));
    let mut body = vec![TYPE_COMMAND];
    tlv_field(&mut body, 1, &5_u32.to_be_bytes());
    tlv_field(&mut body, 2, &command);
    let frame = framed(&body);

    let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
    assert_eq!(
        result.unwrap_err(),
        DecodeError::AppendResourceOutputLimitExceeded
    );
    assert_eq!(
        max, 0,
        "oversized append must fail before the owned bytes allocation"
    );
}

#[test]
fn append_resource_output_declared_length_past_frame_rejects_without_reserving() {
    // A u32 length prefix far larger than the bytes present must error on the
    // leaf read (LengthOverflow / EOF), never reserve.
    let mut command = vec![0x1au8];
    command.push(0);
    command.extend_from_slice(&1_u32.to_be_bytes());
    command.extend_from_slice(&u32::MAX.to_be_bytes());
    command.extend_from_slice(b"abc");
    let mut body = vec![TYPE_COMMAND];
    tlv_field(&mut body, 1, &5_u32.to_be_bytes());
    tlv_field(&mut body, 2, &command);
    let frame = framed(&body);

    let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
    assert!(matches!(
        result,
        Err(DecodeError::LengthOverflow | DecodeError::UnexpectedEof)
    ));
    assert!(max < 1 << 20, "declared-length append reserved {max} bytes");
}

#[test]
fn snapshot_huge_resource_facet_count_does_not_over_reserve() {
    // ATTACHED (0x81): a well-formed empty snapshot followed by a trailing
    // resource-facet list declaring u32::MAX rows and carrying none.
    let mut snap = Vec::new();
    snap.extend_from_slice(&0_u32.to_be_bytes()); // sessions
    snap.extend_from_slice(&0_u32.to_be_bytes()); // windows
    snap.extend_from_slice(&0_u32.to_be_bytes()); // panes
    snap.extend_from_slice(&1_u32.to_be_bytes()); // focused_session
    snap.extend_from_slice(&1_u32.to_be_bytes()); // focused_window
    snap.push(0); // focused_resource tag local
    snap.extend_from_slice(&1_u32.to_be_bytes());
    snap.extend_from_slice(&u32::MAX.to_be_bytes()); // facet rows: huge
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap);
    tlv_field(&mut fields, 2, &7_u32.to_be_bytes());
    tlv_field(&mut fields, 3, &1_u32.to_be_bytes());
    let frame = framed_tlv(0x81, &fields);
    let max = largest_alloc_during(&frame);
    assert!(FrameKind::decode(&frame).is_err());
    assert!(max < 1 << 20, "facet rows reserved {max} bytes");
}

#[test]
fn oversized_history_status_cursors_reject_before_owned_copy() {
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    let cursor = vec![0xA5; MAX_HISTORY_CURSOR_BYTES + 1];
    for type_byte in [TYPE_HISTORY_TOMBSTONE, TYPE_HISTORY_REJECTED] {
        let mut body = vec![type_byte];
        tlv_field(&mut body, 1, &terminal);
        tlv_field(&mut body, 2, &1_u64.to_be_bytes());
        tlv_field(&mut body, 3, &1_u64.to_be_bytes());
        tlv_field(&mut body, 4, &cursor);
        tlv_field(&mut body, 5, &[0]);
        if type_byte == TYPE_HISTORY_REJECTED {
            tlv_field(&mut body, 6, &4096_u32.to_be_bytes());
            tlv_field(&mut body, 7, &256_u32.to_be_bytes());
        }
        let frame = framed(&body);

        let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
        assert_eq!(result.unwrap_err(), DecodeError::BootstrapLimitExceeded);
        assert_eq!(max, 0, "oversized cursor must fail before Bytes allocation");
    }
}

#[test]
fn malformed_history_page_scalars_reject_before_payload_allocation_in_any_field_order() {
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    let payload = vec![0xA5; 256 * 1024];

    for (page_seq, rows, expected) in [
        (None::<u64>, Some(1_u32), DecodeError::UnexpectedEof),
        (
            Some(0_u64),
            Some(1_u32),
            DecodeError::InvalidHistoryPageSequence,
        ),
        (
            Some(1_u64),
            Some(MAX_HISTORY_PAGE_ROWS + 1),
            DecodeError::HistoryRowLimitExceeded,
        ),
    ] {
        let mut body = vec![TYPE_HISTORY_PAGE];
        tlv_field(&mut body, 6, &payload);
        tlv_field(&mut body, 1, &terminal);
        tlv_field(&mut body, 2, &1_u64.to_be_bytes());
        tlv_field(&mut body, 3, &1_u64.to_be_bytes());
        tlv_field(&mut body, 4, b"cursor");
        if let Some(page_seq) = page_seq {
            tlv_field(&mut body, 7, &page_seq.to_be_bytes());
        }
        if let Some(rows) = rows {
            tlv_field(&mut body, 8, &rows.to_be_bytes());
        }
        let frame = framed(&body);

        let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
        assert_eq!(result.unwrap_err(), expected);
        assert_eq!(
            max, 0,
            "malformed page scalar must fail before opaque payload allocation"
        );
    }
}

/// Filler that makes a hostile frame large enough that "at most one element
/// per remaining byte" is still an amplification: the reservation must be
/// bounded by the input's bytes, not its byte count times the element size.
/// `0xFF` makes the first element fail to decode, so only the up-front
/// reservation is measured.
const FILLER: usize = 256 * 1024;

/// Assert a hostile frame never reserves more than the bytes it carries.
fn assert_alloc_within_input(frame: &[u8], what: &str) {
    let max = largest_alloc_during(frame);
    assert!(
        max <= frame.len(),
        "{what}: decoder reserved {max} bytes for a {}-byte frame",
        frame.len()
    );
}

#[test]
fn padded_huge_counts_reserve_no_more_than_the_input() {
    // ATTACHED snapshot: a u32::MAX sessions count followed by filler.
    let mut snap_value = u32::MAX.to_be_bytes().to_vec();
    snap_value.resize(4 + FILLER, 0xFF);
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap_value);
    assert_alloc_within_input(&framed_tlv(0x81, &fields), "attached sessions list");

    // SPAWN_RESOURCE command list: a u32::MAX count followed by filler.
    let mut cmd_value = u32::MAX.to_be_bytes().to_vec();
    cmd_value.resize(4 + FILLER, 0xFF);
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u32.to_be_bytes());
    tlv_field(&mut fields, 2, &1u32.to_be_bytes());
    tlv_field(&mut fields, 3, &cmd_value);
    assert_alloc_within_input(&framed_tlv(0x22, &fields), "spawn command list");

    // SPAWN_RESOURCE env list: pairs are the widest string element.
    let mut env_value = u32::MAX.to_be_bytes().to_vec();
    env_value.resize(4 + FILLER, 0xFF);
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u32.to_be_bytes());
    tlv_field(&mut fields, 2, &1u32.to_be_bytes());
    tlv_field(&mut fields, 5, &env_value);
    assert_alloc_within_input(&framed_tlv(0x22, &fields), "spawn env list");
}

/// The organization bounds (`docs/spec/L1.md` §5.7) are enforced at decode:
/// a name over 256 bytes, an arrangement over 64 KiB, a bulk move or undo
/// over 256 items, an attribute list over 64, and a cursor over 4 KiB are
/// each refused, and a hostile arrangement group count reserves no more
/// than the bytes present.
#[allow(clippy::missing_const_for_fn, reason = "test helpers")]
mod organization_bounds {
    use phux_protocol::ids::{ArrangementKey, IdempotencyKey, ResourceKey, Revision, SessionKey};
    use phux_protocol::wire::DecodeError;
    use phux_protocol::wire::frame::{
        ArrangementDoc, ArrangementGroup, ArrangementNode, Command, EntryRef, EntryTarget,
        FrameKind, MAX_ARRANGEMENT_BYTES, MAX_ORG_ATTRS, MAX_ORG_BATCH, MAX_ORG_CURSOR_BYTES,
        MAX_ORG_NAME_BYTES, MoveItem, OrgFilter, RecordRevision, SessionLifetime,
    };

    use super::{ALLOC_CEILING, largest_alloc_during};

    fn op() -> IdempotencyKey {
        IdempotencyKey::new([1; 16]).unwrap()
    }

    fn session() -> SessionKey {
        SessionKey::new([2; 16]).unwrap()
    }

    fn frame(command: Command) -> Vec<u8> {
        let mut out = bytes::BytesMut::new();
        FrameKind::Command {
            request_id: 1,
            command,
        }
        .encode(&mut out);
        out.to_vec()
    }

    fn decodes(command: Command) -> Result<(), DecodeError> {
        FrameKind::decode(&frame(command)).map(drop)
    }

    fn create(name: String, attrs: usize) -> Command {
        Command::SessionCreate {
            operation: op(),
            name,
            lifetime: SessionLifetime::Persistent,
            attrs: (0..attrs)
                .map(|i| (format!("k{i}"), "v".to_owned()))
                .collect(),
            launch: None,
        }
    }

    fn leaf(i: u32) -> ArrangementNode {
        let mut key = [7; 16];
        key[..4].copy_from_slice(&i.to_be_bytes());
        ArrangementNode::Leaf(EntryRef {
            entry: key,
            target: EntryTarget::Member(ResourceKey::new(key).unwrap()),
        })
    }

    fn replace(groups: u32) -> Command {
        Command::ArrangementReplace {
            operation: op(),
            arrangement: ArrangementKey::new([3; 16]).unwrap(),
            expected: Revision::new(1),
            document: ArrangementDoc {
                groups: (0..groups)
                    .map(|i| ArrangementGroup {
                        id: [9; 16],
                        name: None,
                        tree: leaf(i),
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn names_and_attribute_lists_are_bounded() {
        assert_eq!(decodes(create("a".repeat(MAX_ORG_NAME_BYTES), 0)), Ok(()));
        assert_eq!(
            decodes(create("a".repeat(MAX_ORG_NAME_BYTES + 1), 0)),
            Err(DecodeError::OrganizationLimitExceeded)
        );
        assert_eq!(decodes(create("w".to_owned(), MAX_ORG_ATTRS)), Ok(()));
        assert_eq!(
            decodes(create("w".to_owned(), MAX_ORG_ATTRS + 1)),
            Err(DecodeError::OrganizationLimitExceeded)
        );
    }

    #[test]
    fn arrangement_documents_are_bounded_at_64_kib() {
        // One leaf group is 16 + 1 + 1 + 16 + 1 + 16 = 51 bytes.
        let fits = u32::try_from((MAX_ARRANGEMENT_BYTES - 2) / 51).unwrap();
        assert_eq!(decodes(replace(fits)), Ok(()));
        assert_eq!(
            decodes(replace(fits + 1)),
            Err(DecodeError::OrganizationLimitExceeded)
        );
    }

    #[test]
    fn batches_and_cursors_are_bounded() {
        let items = |n: usize| Command::ResourceMoveMany {
            operation: op(),
            items: (0..n)
                .map(|_| MoveItem {
                    resource: ResourceKey::new([4; 16]).unwrap(),
                    from: session(),
                    from_expected: Revision::new(1),
                })
                .collect(),
            to: SessionKey::new([5; 16]).unwrap(),
            to_expected: Revision::new(1),
        };
        assert_eq!(decodes(items(MAX_ORG_BATCH)), Ok(()));
        assert_eq!(
            decodes(items(MAX_ORG_BATCH + 1)),
            Err(DecodeError::OrganizationLimitExceeded)
        );
        let undo = |n: usize| Command::OrganizationUndo {
            operation: op(),
            undo: IdempotencyKey::new([6; 16]).unwrap(),
            expected: (0..n)
                .map(|_| RecordRevision {
                    key: [8; 16],
                    revision: Revision::new(1),
                })
                .collect(),
        };
        assert_eq!(
            decodes(undo(MAX_ORG_BATCH + 1)),
            Err(DecodeError::OrganizationLimitExceeded)
        );
        let page = |n: usize| Command::GetOrganization {
            filter: OrgFilter::Intake,
            page_size: 10,
            cursor: Some(vec![0; n]),
        };
        assert_eq!(decodes(page(MAX_ORG_CURSOR_BYTES)), Ok(()));
        assert_eq!(
            decodes(page(MAX_ORG_CURSOR_BYTES + 1)),
            Err(DecodeError::OrganizationLimitExceeded)
        );
    }

    #[test]
    fn hostile_group_count_does_not_over_reserve() {
        let mut bytes = frame(replace(1));
        // The document is the last field: rewrite its u16 group count, which
        // sits 2 bytes after the document field's length prefix, to u16::MAX
        // by locating the single group's id run.
        let id_at = bytes
            .windows(16)
            .position(|w| w == [9; 16])
            .expect("the group id is in the frame");
        bytes[id_at - 2..id_at].copy_from_slice(&u16::MAX.to_be_bytes());
        let max = largest_alloc_during(&bytes);
        assert!(FrameKind::decode(&bytes).is_err());
        assert!(max < ALLOC_CEILING, "reserved {max} bytes");
    }
}
