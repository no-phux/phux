//! Run alone in its own process: allocation controls are process-wide, so a
//! library test would interfere with unrelated tests' dev-mode assertions.

#[global_allocator]
static ALLOC: phux_perf::alloc::Counting = phux_perf::alloc::Counting::system();

#[gpui::test]
fn allocation_counting_toggles_live_without_enabling_dev_mode(cx: &mut gpui::TestAppContext) {
    use phux_desktop::devtools;
    use phux_perf::alloc;

    assert!(alloc::installed());
    assert!(!alloc::enabled());
    assert!(alloc::stats().is_none());
    cx.update(|cx| devtools::set_allocation_counting(true, cx));
    let before = alloc::stats().expect("enabled counter");
    let block = std::hint::black_box(vec![0x5a_u8; 32 * 1024]);
    let during = alloc::stats().expect("live counter");
    assert!(during.alloc_bytes >= before.alloc_bytes + block.len() as u64);
    assert!(during.allocs > before.allocs);
    drop(block);
    assert!(alloc::stats().unwrap().frees > during.frees);
    assert!(!devtools::get().dev);
    assert!(devtools::get().socket.get().is_none());
    cx.update(|cx| devtools::set_allocation_counting(false, cx));
    assert!(!alloc::enabled());
    assert!(alloc::stats().is_none());
    assert!(!devtools::get().dev);
    cx.update(|cx| devtools::set_allocation_counting(true, cx));
    assert!(
        alloc::stats().is_some(),
        "counting can start a new live interval"
    );
    cx.update(|cx| devtools::set_allocation_counting(false, cx));
}
