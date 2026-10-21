//! Callbacks, perf, and search: re-entry guards, panics, and borrowed results.

use super::*;
use std::ffi::c_void;

struct CallbackContext {
    client: *mut PhuxClient,
    calls: usize,
    reentry_result: PhuxClientResult,
    failure_result: PhuxClientResult,
    failure_message: Vec<u8>,
}

unsafe extern "C-unwind" fn attached_callback(userdata: *mut c_void) {
    let context = unsafe { &mut *userdata.cast::<CallbackContext>() };
    context.calls += 1;
    context.reentry_result = unsafe { phux_client_outgoing_clear(context.client) };
}

unsafe extern "C-unwind" fn failure_callback(
    userdata: *mut c_void,
    result: PhuxClientResult,
    message: PhuxBytes,
) {
    let context = unsafe { &mut *userdata.cast::<CallbackContext>() };
    context.calls += 1;
    context.failure_result = result;
    context.failure_message = unsafe { bytes_in(message.data, message.len) }
        .expect("callback message span")
        .to_vec();
    context.reentry_result = unsafe { phux_client_outgoing_clear(context.client) };
}

unsafe extern "C-unwind" fn panic_callback(_: *mut c_void) {
    panic!("callback panic");
}

/// The perf report carries the runtime's owner-thread rows beside the
/// kernel's, so an embedder can see its publications per frame.
#[test]
fn perf_json_reports_kernel_and_runtime_metrics() {
    let client = new_client();
    let mut out = PhuxBytes::default();
    // SAFETY: a live client from `new_client` and a writable output.
    assert_eq!(
        unsafe { phux_client_perf_json(client, &raw mut out) },
        PhuxClientResult::Ok
    );
    // SAFETY: the bytes are borrowed from the live client.
    let json = unsafe { bytes_in(out.data, out.len) }.expect("perf json span");
    let json = std::str::from_utf8(json).expect("utf-8");
    for name in ["kernel.frames", "runtime.apply_batches", "runtime.publish"] {
        assert!(
            json.contains(&format!("\"{name}\"")),
            "{name} missing: {json}"
        );
    }
    // SAFETY: the client is released exactly once.
    unsafe { phux_client_free(client) };
}

#[test]
fn local_projection_budget_may_be_smaller_than_authenticated_page_rows() {
    let options = PhuxClientOptions {
        size: mem::size_of::<PhuxClientOptions>(),
        version: ABI_VERSION,
        max_bootstrap_chunk_bytes: 1024,
        max_history_page_bytes: 1024,
        max_history_page_rows: 1024,
        max_history_cache_bytes: 4096,
        max_history_materialized_rows: 1,
        history_prefetch_rows: 2,
    };
    assert!(!history_cache_cannot_retain_page(&options));
    assert!(client_limits(&options).is_ok());
}

#[test]
fn search_result_release_consumes_the_borrowed_set_in_one_mutation() {
    let client = new_client();
    unsafe {
        (*client).inner.search_results.push(PhuxSearchResult {
            start: PhuxDocumentAnchor { opaque_id: 41 },
            end: PhuxDocumentAnchor { opaque_id: 42 },
        });
        assert_eq!(
            phux_client_search_results_release(client),
            PhuxClientResult::Ok
        );
        assert!((*client).inner.search_results.is_empty());
        assert_eq!(
            phux_client_search_results_release(client),
            PhuxClientResult::Ok
        );

        phux_client_free(client);
    }
}

#[test]
fn attached_callback_runs_after_staging_once_and_rejects_reentry() {
    let client = attaching(&[], 7);
    let mut context = CallbackContext {
        client,
        calls: 0,
        reentry_result: PhuxClientResult::Ok,
        failure_result: PhuxClientResult::Ok,
        failure_message: Vec::new(),
    };
    unsafe {
        (*client).inner.callbacks = PhuxClientCallbacks {
            userdata: ptr::from_mut(&mut context).cast(),
            on_attached: Some(attached_callback),
            ..PhuxClientCallbacks::default()
        };
    }
    complete_resource_attach(
        client,
        single_terminal_snapshot(phux_protocol::ResourceId::local(7), 80, 24),
    );
    assert_eq!(
        feed(client, &FrameKind::Pong { nonce: 1 }),
        PhuxClientResult::Ok
    );
    assert_eq!(context.calls, 1);
    assert_eq!(context.reentry_result, PhuxClientResult::InvalidState);
    unsafe { phux_client_free(client) };
}

#[test]
fn failure_callback_observes_stable_error_and_rejects_reentry() {
    let client = new_client();
    let mut context = CallbackContext {
        client,
        calls: 0,
        reentry_result: PhuxClientResult::Ok,
        failure_result: PhuxClientResult::Ok,
        failure_message: Vec::new(),
    };
    unsafe {
        (*client).inner.callbacks = PhuxClientCallbacks {
            userdata: ptr::from_mut(&mut context).cast(),
            on_failure: Some(failure_callback),
            ..PhuxClientCallbacks::default()
        };
    }
    let result = with_client_mut(client, |_| Err(BridgeError::invalid("exact failure")));
    assert_eq!(result, PhuxClientResult::InvalidArgument);
    assert_eq!(context.calls, 1);
    assert_eq!(context.failure_result, PhuxClientResult::InvalidArgument);
    assert_eq!(context.failure_message, b"exact failure");
    assert_eq!(context.reentry_result, PhuxClientResult::InvalidState);
    unsafe { phux_client_free(client) };
}

#[test]
fn callback_panic_is_contained_and_clears_reentry_guard() {
    const CHILD: &str = "PHUX_CLIENT_FFI_PANIC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status =
            std::process::Command::new(std::env::current_exe().expect("current test executable"))
                .args([
                    "--exact",
                    "c::tests::callback_panic_is_contained_and_clears_reentry_guard",
                ])
                .env(CHILD, "1")
                .status()
                .expect("spawn panic-containment host process");
        assert!(status.success(), "panic-containment host process aborted");
        return;
    }

    let client = attaching(&[], 7);
    unsafe {
        (*client).inner.callbacks = PhuxClientCallbacks {
            on_attached: Some(panic_callback),
            ..PhuxClientCallbacks::default()
        };
    }
    let terminal = phux_protocol::ResourceId::local(7);
    assert_eq!(
        feed(
            client,
            &attached_frame(7, single_terminal_snapshot(terminal.clone(), 80, 24))
        ),
        PhuxClientResult::Ok
    );
    feed_bootstrap(client, &terminal, (1, 1), (80, 24), b"");
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 7 }),
        PhuxClientResult::Panic
    );
    assert_eq!(
        unsafe { phux_client_outgoing_clear(client) },
        PhuxClientResult::Ok
    );
    unsafe { phux_client_free(client) };
}
