//! Compile-time uniqueness of wire discriminants (phux-ke0c).
//!
//! `src/wire/frame/mod.rs` declares every discriminant through `wire_tags!`,
//! which reserves each byte as a trait impl in its namespace. These tests
//! compile the production declarations in isolation with `rustc`, so the
//! reservation is proven without relying on decoder match arms or on a second
//! list of allocated bytes.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "test harness helpers fail loudly and report a skipped probe on stderr"
)]

use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, Output};

/// The compiler that built this test: `RUSTC` when set, else the `rustc`
/// beside the `cargo` running the test, else `rustc` on `PATH`.
fn rustc() -> PathBuf {
    if let Some(rustc) = std::env::var_os("RUSTC") {
        return rustc.into();
    }
    let cargo = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO").map(PathBuf::from));
    if let Some(cargo) = cargo {
        let sibling = cargo.with_file_name(format!("rustc{}", std::env::consts::EXE_SUFFIX));
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from("rustc")
}

/// Type-checks the frame declarations plus `extra`. `None` means no compiler
/// could be found, which is reported on stderr as a skipped probe.
fn compile_declarations(extra: &str) -> Option<Output> {
    let declarations = include_str!("../src/wire/frame/mod.rs")
        .split_once("\nmod codec;")
        .expect("frame declarations precede the codec modules")
        .0;
    let temp = tempfile::tempdir().expect("create probe directory");
    let source = temp.path().join("wire_tag_probe.rs");
    std::fs::write(&source, format!("{declarations}\n{extra}\n")).expect("write probe source");
    let rustc = rustc();
    let output = Command::new(&rustc)
        .args([
            "--edition=2024",
            "--crate-type=lib",
            "--crate-name=wire_tag_probe",
            "--emit=metadata",
            "--cap-lints=allow",
        ])
        .arg("--out-dir")
        .arg(temp.path())
        .arg(&source)
        .output();
    match output {
        Ok(output) => Some(output),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            eprintln!(
                "SKIPPED: no rustc at {} ({error}); wire-tag compile probes did not run",
                rustc.display()
            );
            None
        }
        Err(error) => panic!("failed to run {}: {error}", rustc.display()),
    }
}

fn assert_compiles(extra: &str) {
    let Some(output) = compile_declarations(extra) else {
        return;
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn declarations_compile_and_distinct_namespaces_may_reuse_bytes() {
    assert_compiles("");
}

#[test]
fn an_unallocated_byte_may_be_declared() {
    // 0x56 is open in the L3 client-to-server block (`0x56..=0x5F`). This
    // control proves the duplicate probes below fail on the collision alone.
    assert_compiles("wire_tags! { Message; const FUTURE_TAG: u8 = 0x56; }");
}

#[test]
fn duplicate_tags_fail_even_when_reserved_or_declared_in_another_block() {
    // `FUTURE_TAG` has no decoder arm; the collision must still be rejected.
    // TYPE_WORKLOAD_CHALLENGE is gone with the retired in-band workload-auth
    // wire (ADR-0116); TYPE_FRAME_COMPRESSED covers that Message block.
    for (namespace, existing) in [
        ("Message", "TYPE_HELLO"),
        ("Message", "TYPE_FRAME_COMPRESSED"),
        ("Message", "TYPE_DIRECTORY_LISTING"),
        ("Message", "TYPE_EVENT"),
        ("Command", "COMMAND_TAG_APPEND_RESOURCE_OUTPUT"),
        ("SpawnError", "SPAWN_ERROR_TAG_PARENT_KIND_MISMATCH"),
        ("Event", "EVENT_TAG_ASKED"),
        ("AttachTarget", "ATTACH_TARGET_CREATE_IF_MISSING"),
    ] {
        let Some(output) = compile_declarations(&format!(
            "wire_tags! {{ {namespace}; const FUTURE_TAG: u8 = {existing}; }}"
        )) else {
            return;
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "duplicate of {existing} compiled");
        assert!(stderr.contains("E0119"), "{existing}: {stderr}");
    }
}
