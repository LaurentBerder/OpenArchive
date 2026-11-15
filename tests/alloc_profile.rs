//! Allocation profiling harness. Run with:
//!
//! ```text
//! cargo test --release --test alloc_profile -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The counters are global, so this test is intentionally ignored and must run
//! alone. It measures allocation count and peak live bytes for archive
//! creation and extraction; it does not assert thresholds because allocator
//! behaviour varies by platform and Rust version.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use openarchive::core::{CompressionOptions, ExtractOptions};
use openarchive::formats::{create_archive, get_reader};

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            let live = LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[derive(Debug, Clone, Copy)]
struct Snapshot {
    allocations: usize,
    live: usize,
    peak: usize,
}

fn snapshot() -> Snapshot {
    Snapshot {
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        live: LIVE_BYTES.load(Ordering::Relaxed),
        peak: PEAK_BYTES.load(Ordering::Relaxed),
    }
}

fn measure(label: &str, operation: impl FnOnce()) {
    PEAK_BYTES.store(LIVE_BYTES.load(Ordering::Relaxed), Ordering::Relaxed);
    let before = snapshot();
    operation();
    let after = snapshot();

    println!(
        "{label:<28} allocations={:<8} peak_delta={:>8.1} KiB",
        after.allocations.saturating_sub(before.allocations),
        after.peak.saturating_sub(before.live) as f64 / 1024.0
    );
}

fn make_fixture(root: &Path, count: usize, size: usize) -> PathBuf {
    let data_dir = root.join("data");
    fs::create_dir_all(&data_dir).unwrap();
    let payload = vec![0x5Au8; size];
    for index in 0..count {
        fs::write(data_dir.join(format!("file_{index:04}.bin")), &payload).unwrap();
    }
    data_dir
}

#[test]
#[ignore = "allocation profiling harness; run with --ignored --nocapture --test-threads=1"]
fn profile_allocations() {
    let root = std::env::temp_dir().join(format!("openarchive_alloc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let data_dir = make_fixture(&root, 400, 4096);
    let sources = vec![data_dir.clone()];
    let compression = CompressionOptions {
        level: 5,
        threads: Some(1),
        ..Default::default()
    };

    for extension in ["zip", "7z"] {
        let archive = root.join(format!("profile.{extension}"));
        measure(&format!("create {extension} (1 thread)"), || {
            create_archive(&archive, &sources, &compression).unwrap();
        });
    }

    for extension in ["zip", "7z"] {
        let archive = root.join(format!("profile.{extension}"));
        for threads in [1usize, 8] {
            let output_dir = root.join(format!("extract_{extension}_{threads}"));
            let options = ExtractOptions {
                output_dir,
                threads: Some(threads),
                ..Default::default()
            };
            measure(&format!("extract {extension} ({threads} threads)"), || {
                let mut reader = get_reader(&archive, None).unwrap();
                reader.extract(&options, None).unwrap();
            });
        }
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
#[ignore = "allocation profiling harness; run with --ignored --nocapture --test-threads=1"]
fn profile_medium_files() {
    let root =
        std::env::temp_dir().join(format!("openarchive_alloc_medium_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    // 200 × 128 KiB: large enough to use the buffered I/O path, small enough
    // to keep the run short.
    let data_dir = make_fixture(&root, 200, 128 * 1024);
    let sources = vec![data_dir];
    let archive = root.join("profile.zip");
    let compression = CompressionOptions {
        level: 5,
        threads: Some(1),
        ..Default::default()
    };

    measure("create zip medium (1 thread)", || {
        create_archive(&archive, &sources, &compression).unwrap();
    });

    for threads in [1usize, 8] {
        let options = ExtractOptions {
            output_dir: root.join(format!("extract_medium_{threads}")),
            threads: Some(threads),
            ..Default::default()
        };
        measure(&format!("extract zip medium ({threads} threads)"), || {
            let mut reader = get_reader(&archive, None).unwrap();
            reader.extract(&options, None).unwrap();
        });
    }

    let _ = fs::remove_dir_all(&root);
}
