//! `phux-desktop`: see [`phux_desktop::launch`].
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

/// Counts nothing until dev mode enables it ([`phux_desktop::devtools`]).
#[global_allocator]
static ALLOC: phux_perf::alloc::Counting = phux_perf::alloc::Counting::system();

fn main() -> std::process::ExitCode {
    phux_desktop::launch::main()
}
