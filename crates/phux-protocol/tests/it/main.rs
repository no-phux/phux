//! phux-protocol integration tests: wire contracts, frame snapshots,
//! malformed-input rejection, and the allocation gates, in one binary.

mod common;

mod decode_fuzz;
mod file_transfer_snapshots;
mod frame_wire_snapshots;
mod hot_path_alloc;
mod kinds_table;
mod org_wire_snapshots;
mod scope_encoding;
mod wire_alloc_dos;
mod wire_contract;
mod wire_tag_registry;

/// The binary's global allocator. It forwards every call to `System` and
/// reports each request size to the two allocation gates, whose thread-local
/// switches decide whether the request is counted.
mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};

    struct Probe;

    fn observe(size: usize) {
        crate::hot_path_alloc::note(size);
        crate::wire_alloc_dos::record(size);
    }

    // SAFETY: forwards every call to `System` unchanged; the only addition is
    // bumping thread-local `Cell`s, which never touch the returned memory.
    unsafe impl GlobalAlloc for Probe {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            observe(layout.size());
            // SAFETY: the caller upholds `alloc`'s layout contract.
            unsafe { System.alloc(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            observe(new_size);
            // SAFETY: the caller upholds `realloc`'s pointer/layout contract.
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: the caller upholds `dealloc`'s pointer/layout pairing.
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static A: Probe = Probe;
}
