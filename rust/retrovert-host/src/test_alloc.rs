//! Allocation assertions for steady-state unit tests.
//!
//! Counting is enabled only on the current thread while an assertion runs. This keeps
//! unrelated tests and the test harness from making real-time checks flaky when the test
//! runner executes cases concurrently.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAllocator;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn record_allocation() {
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            let _ = ALLOCATIONS.try_with(|allocations| {
                allocations.set(allocations.get().saturating_add(1));
            });
        }
    });
}

// SAFETY: every operation is forwarded unchanged to `System`; the wrapper only updates
// thread-local counters before operations which can create or grow an allocation.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: the caller supplies the layout required by `GlobalAlloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: the caller supplies the layout required by `GlobalAlloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller supplies the pointer and layout required by `GlobalAlloc`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation();
        // SAFETY: the caller supplies the pointer, layout and size required by `GlobalAlloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct CountingGuard;

impl Drop for CountingGuard {
    fn drop(&mut self) {
        let _ = COUNTING.try_with(|counting| counting.set(false));
    }
}

/// Fails when `body` creates or grows a heap allocation on the current thread.
pub(crate) fn assert_no_alloc(body: impl FnOnce()) {
    COUNTING.with(|counting| {
        assert!(!counting.replace(true), "nested allocation measurement");
    });
    ALLOCATIONS.with(|allocations| allocations.set(0));
    let guard = CountingGuard;

    body();

    drop(guard);
    let allocations = ALLOCATIONS.with(Cell::get);
    assert_eq!(allocations, 0, "steady-state path allocated");
}
