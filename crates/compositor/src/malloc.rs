//! glibc's mmap threshold.
//!
//! The compositor's large allocations are short-lived pixel buffers: a
//! monitor change's snapshot of the old screen, a wallpaper decoded for
//! the new layout, screenshot and screencast frames. glibc serves blocks
//! at or above its mmap threshold with their own mapping and unmaps them
//! when freed, but by default it raises the threshold to the size of every
//! mmapped block freed (up to 32 MiB on 64-bit). After the first freed
//! full-screen buffer the next ones come from the heap and the worker
//! threads' arenas instead, and their pages stay resident after they are
//! freed: a burst of monitor changes left the session over 100 MiB larger
//! for good. Pinning the threshold turns that adjustment off, so a buffer
//! this size is returned to the kernel when it is dropped.

/// Blocks at least this large get their own mapping: a quarter of a
/// 1280x800 XRGB8888 frame, above the per-frame scratch allocations that
/// stay in the heap.
pub const MMAP_THRESHOLD: usize = 1024 * 1024;

/// Pin glibc's mmap threshold at [`MMAP_THRESHOLD`]. Returns whether
/// glibc accepted it (other C libraries do not adjust a threshold).
pub fn pin_mmap_threshold() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: mallopt takes plain integers and has no memory effects
        // beyond changing the allocator's parameters.
        unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD as libc::c_int) == 1 }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        false
    }
}
