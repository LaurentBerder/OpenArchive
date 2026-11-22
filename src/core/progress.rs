//! Progress reporting and cancellation for long archive operations.

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Shared between an operation and whoever watches it (the GUI). The
/// operation reports bytes as it writes or verifies them and checks for
/// cancellation; the watcher reads the counters and may cancel.
#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the number of bytes the operation expects to process. Formats
    /// that cannot know it in advance (streamed TAR) leave it unset.
    pub(crate) fn set_total(&self, bytes: u64) {
        self.total.store(bytes, Ordering::Relaxed);
    }

    pub(crate) fn add(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Bytes processed so far.
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    /// Bytes expected in total, if known.
    pub fn total(&self) -> Option<u64> {
        Some(self.total.load(Ordering::Relaxed)).filter(|&total| total > 0)
    }

    /// Completed fraction in `0.0..=1.0`, if the total is known.
    pub fn fraction(&self) -> Option<f32> {
        self.total()
            .map(|total| (self.done() as f64 / total as f64).clamp(0.0, 1.0) as f32)
    }

    /// Asks the operation to stop at its next check.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Returns a [`Cancelled`] error once [`Progress::cancel`] was called.
    pub(crate) fn check(&self) -> io::Result<()> {
        if self.is_cancelled() {
            Err(io::Error::new(io::ErrorKind::Interrupted, Cancelled))
        } else {
            Ok(())
        }
    }
}

/// The error an operation stops with after being cancelled. It travels inside
/// an `io::Error` of kind `Interrupted`; use `formats::is_cancelled` to
/// recognise it through the format libraries' wrappers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("operation cancelled")
    }
}

impl std::error::Error for Cancelled {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_needs_a_total() {
        let progress = Progress::new();
        progress.add(10);
        assert_eq!(progress.fraction(), None);
        progress.set_total(40);
        assert_eq!(progress.fraction(), Some(0.25));
        progress.add(100);
        assert_eq!(progress.fraction(), Some(1.0));
    }

    #[test]
    fn check_fails_only_after_cancel() {
        let progress = Progress::new();
        assert!(progress.check().is_ok());
        progress.cancel();
        let err = progress.check().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(err.get_ref().unwrap().is::<Cancelled>());
    }
}
