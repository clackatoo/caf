//! What caf-tools allocates at most for its biggest inputs: a test binary
//! of its own, as it counts every allocation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use caf_tools::{EditError, FileRead, MOST_EDIT, ReadState, Version, edit};

/// The system's allocator, keeping the most it ever had out.
struct Counting;

static NOW: AtomicUsize = AtomicUsize::new(0);
static MOST: AtomicUsize = AtomicUsize::new(0);

// SAFETY: it passes every call on to the system's allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = NOW.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        MOST.fetch_max(now, Ordering::Relaxed);
        // SAFETY: as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        NOW.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn counting_a_string_in_a_full_size_file_takes_little_memory() {
    let size = usize::try_from(MOST_EDIT).unwrap();
    let file = FileRead::new(vec![b'a'; size], Version::new("1"));
    let mut state = ReadState::new();
    state.record("/a".into(), Version::new("1"), &file.bytes, false);
    let before = NOW.load(Ordering::Relaxed);
    MOST.store(before, Ordering::Relaxed);
    let error = edit(Path::new("/a"), Some(&file), &state, "a", "b", false).unwrap_err();
    assert!(matches!(error, EditError::Several { count, .. } if count == size));
    let used = MOST.load(Ordering::Relaxed) - before;
    // A copy or two of the file, never a range for every match.
    assert!(used <= 4 * size, "used {used} bytes for a {size}-byte file");
}
