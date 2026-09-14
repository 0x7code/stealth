//! Heap telemetry used to budget PSRAM between Gallery and ESP-DL detection modes.

use esp_idf_svc::sys;

/// Log total and largest-contiguous free blocks for the two heaps relevant to this application.
///
/// The largest PSRAM block is the practical upper bound for a future contiguous tensor arena;
/// total free memory alone can hide heap fragmentation.
pub fn log_free(label: &str) {
    let internal = heap(sys::MALLOC_CAP_INTERNAL);
    let psram = heap(sys::MALLOC_CAP_SPIRAM);
    log::info!(
        "memory [{label}]: internal={} KiB (largest={} KiB), PSRAM={} KiB (largest={} KiB)",
        internal.free / 1024,
        internal.largest / 1024,
        psram.free / 1024,
        psram.largest / 1024,
    );
}

struct Heap {
    free: usize,
    largest: usize,
}

fn heap(capability: u32) -> Heap {
    // ESP-IDF's heap-capability queries are read-only and safe to call from this task.
    Heap {
        free: unsafe { sys::heap_caps_get_free_size(capability) },
        largest: unsafe { sys::heap_caps_get_largest_free_block(capability) },
    }
}
