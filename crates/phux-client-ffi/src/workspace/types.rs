//! Sized C records for shared topology and the emulator-free catalog.
use crate::{ABI_VERSION, PhuxBytes, PhuxResourceId};
use std::mem;

/// Current snapshot and latest transaction result; spans expire on mutation.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxWorkspaceInfo {
    pub size: usize,
    pub version: u32,
    pub revision: u64,
    pub session_id: u32,
    pub state: u32,
    pub window_count: u32,
    pub node_count: u32,
    pub terminal_count: u32,
    pub request_id: u32,
    pub status: u32,
    pub message: PhuxBytes,
}

/// A durable layout window, unrelated to a native host window.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxWorkspaceWindow {
    pub size: usize,
    pub version: u32,
    pub window_id: [u8; 16],
    pub name: PhuxBytes,
    pub root_node: u32,
}

/// Flattened node: leaf 1, side-by-side 2, stacked 3.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxWorkspaceNode {
    pub size: usize,
    pub version: u32,
    pub kind: u32,
    pub terminal_id: PhuxResourceId,
    pub first: u32,
    pub second: u32,
    pub ratio: f32,
}

/// Durable terminal listing without an emulator or subscription.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxCatalogTerminal {
    pub size: usize,
    pub version: u32,
    pub terminal_id: PhuxResourceId,
    pub session_id: u32,
    pub title: PhuxBytes,
    pub cwd: PhuxBytes,
}

/// Typed edit against the exact locally observed revision. See the C header for tags.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxWorkspaceMutation {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub expected_revision: u64,
    pub session_id: u32,
    pub kind: u32,
    pub window_id: [u8; 16],
    pub terminal_id: PhuxResourceId,
    pub new_terminal_id: PhuxResourceId,
    pub name: PhuxBytes,
    pub direction: u32,
    pub index: u32,
    pub ratio: f32,
    pub path_len: u32,
    pub path_bits: u64,
}

macro_rules! defaults {
    ($($ty:ty),*) => {$ (
        impl Default for $ty {
            fn default() -> Self {
                // SAFETY: all fields are integer/float scalars or nullable byte spans;
                // zero is valid for each. The ABI header is initialized immediately.
                let mut value: Self = unsafe { std::mem::zeroed() };
                value.size = mem::size_of::<Self>();
                value.version = ABI_VERSION;
                value
            }
        }
    )*};
}
defaults!(
    PhuxWorkspaceInfo,
    PhuxWorkspaceWindow,
    PhuxWorkspaceNode,
    PhuxCatalogTerminal,
    PhuxWorkspaceMutation
);
