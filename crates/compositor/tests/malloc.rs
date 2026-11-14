//! A freed full-screen pixel buffer goes back to the kernel even after a
//! larger one was freed (see `tuna_compositor::malloc`). Its own binary:
//! glibc's allocator parameters are process-wide.
#![cfg(all(target_os = "linux", target_env = "gnu"))]

#[test]
fn a_frame_buffer_is_unmapped_when_dropped_after_a_larger_one() {
    assert!(tuna_compositor::malloc::pin_mmap_threshold());
    let frame = 1280 * 800 * 4;
    // Unpinned, glibc raises its threshold to this block's size when it is
    // freed, and the next frame-sized buffer would come from the heap.
    drop(std::hint::black_box(vec![1u8; 2 * frame]));
    // SAFETY: mallinfo2 only reads allocator statistics.
    let before = unsafe { libc::mallinfo2() };
    let buffer = std::hint::black_box(vec![1u8; frame]);
    // SAFETY: as above.
    let during = unsafe { libc::mallinfo2() };
    assert_eq!(
        during.hblks,
        before.hblks + 1,
        "a frame-sized buffer gets its own mapping"
    );
    drop(buffer);
    // SAFETY: as above.
    let after = unsafe { libc::mallinfo2() };
    assert_eq!(
        (after.hblks, after.hblkhd),
        (before.hblks, before.hblkhd),
        "and the mapping is gone once it is dropped"
    );
}
