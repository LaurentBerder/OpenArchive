//! What to do when an extracted file already exists.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Policy for files that already exist at their extraction target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverwriteMode {
    /// Replace them (`-aoa`, `-y`).
    Overwrite,
    /// Keep them and do not extract that entry (`-aos`).
    Skip,
    /// Keep them and extract the entry as "name (1).ext" (`-aou`).
    Rename,
    /// Ask a [`Prompter`] for each one (the CLI default).
    Ask,
}

/// An answer to "this file already exists".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Overwrite,
    Skip,
    Rename,
    /// Stop the whole extraction.
    Abort,
}

/// Asks the user about one existing file. Calls are serialised, so an
/// implementation can read the terminal; it may remember "always" answers.
pub trait Prompter: Send + Sync {
    fn ask(&self, existing: &Path) -> Decision;
}

/// The error an extraction stops with when the user chose to quit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aborted;

impl fmt::Display for Aborted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("extraction stopped at the user's request")
    }
}

impl std::error::Error for Aborted {}

/// Resolves extraction targets against existing files for one extraction.
///
/// Every target is decided once and remembered: formats compute the same
/// target in a planning pass and again while writing, possibly on several
/// threads, and the user must be asked only once per file.
pub struct Conflicts {
    mode: OverwriteMode,
    prompter: Option<Arc<dyn Prompter>>,
    state: Mutex<State>,
    skipped: AtomicUsize,
    renamed: AtomicUsize,
}

#[derive(Default)]
struct State {
    /// Target → where to write it, or `None` to skip.
    decided: HashMap<PathBuf, Option<PathBuf>>,
    /// Renamed targets handed out, so two entries never get the same name.
    reserved: HashSet<PathBuf>,
}

impl Conflicts {
    pub fn new(mode: OverwriteMode) -> Self {
        Self {
            mode,
            prompter: None,
            state: Mutex::new(State::default()),
            skipped: AtomicUsize::new(0),
            renamed: AtomicUsize::new(0),
        }
    }

    /// Asks `prompter` about each existing file.
    pub fn asking(prompter: Arc<dyn Prompter>) -> Self {
        Self { prompter: Some(prompter), ..Self::new(OverwriteMode::Ask) }
    }

    pub fn mode(&self) -> OverwriteMode {
        self.mode
    }

    /// Existing files left untouched so far.
    pub fn skipped(&self) -> usize {
        self.skipped.load(Ordering::Relaxed)
    }

    /// Entries written under a new name so far.
    pub fn renamed(&self) -> usize {
        self.renamed.load(Ordering::Relaxed)
    }

    /// Where to write an entry whose target is `target`: the target itself,
    /// a renamed path, or `None` to skip it. Fails with [`Aborted`] when the
    /// user chose to quit.
    pub(crate) fn resolve(&self, target: &Path) -> io::Result<Option<PathBuf>> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(decided) = state.decided.get(target) {
            return Ok(decided.clone());
        }

        let decision = if !target.exists() || target.is_dir() {
            Decision::Overwrite
        } else {
            match self.mode {
                OverwriteMode::Overwrite => Decision::Overwrite,
                OverwriteMode::Skip => Decision::Skip,
                OverwriteMode::Rename => Decision::Rename,
                // Without a prompter there is nobody to ask: keep the file.
                OverwriteMode::Ask => self.prompter.as_ref().map_or(Decision::Skip, |p| p.ask(target)),
            }
        };

        let outcome = match decision {
            Decision::Overwrite => Some(target.to_path_buf()),
            Decision::Skip => {
                self.skipped.fetch_add(1, Ordering::Relaxed);
                None
            }
            Decision::Rename => {
                let renamed = free_name(target, &state.reserved);
                state.reserved.insert(renamed.clone());
                self.renamed.fetch_add(1, Ordering::Relaxed);
                Some(renamed)
            }
            Decision::Abort => return Err(io::Error::new(io::ErrorKind::Interrupted, Aborted)),
        };
        state.decided.insert(target.to_path_buf(), outcome.clone());
        Ok(outcome)
    }
}

impl Default for Conflicts {
    fn default() -> Self {
        Self::new(OverwriteMode::Overwrite)
    }
}

impl fmt::Debug for Conflicts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conflicts")
            .field("mode", &self.mode)
            .field("skipped", &self.skipped())
            .field("renamed", &self.renamed())
            .finish()
    }
}

/// The first "stem (n).ext" next to `path` that neither exists nor was
/// already handed out.
fn free_name(path: &Path, reserved: &HashSet<PathBuf>) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let extension = path.extension().map(|ext| format!(".{}", ext.to_string_lossy())).unwrap_or_default();
    (1..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){extension}")))
        .find(|candidate| !candidate.exists() && !reserved.contains(candidate))
        .expect("an unused name exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openarchive-conflict-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct Scripted(Mutex<Vec<Decision>>, AtomicUsize);
    impl Prompter for Scripted {
        fn ask(&self, _existing: &Path) -> Decision {
            self.1.fetch_add(1, Ordering::Relaxed);
            self.0.lock().unwrap().remove(0)
        }
    }

    #[test]
    fn missing_targets_are_written_in_every_mode() {
        let dir = scratch("missing");
        for mode in [OverwriteMode::Overwrite, OverwriteMode::Skip, OverwriteMode::Rename, OverwriteMode::Ask] {
            let target = dir.join("new.txt");
            assert_eq!(Conflicts::new(mode).resolve(&target).unwrap(), Some(target));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn modes_apply_to_existing_files() {
        let dir = scratch("modes");
        let existing = dir.join("a.txt");
        std::fs::write(&existing, b"old").unwrap();

        assert_eq!(Conflicts::new(OverwriteMode::Overwrite).resolve(&existing).unwrap(), Some(existing.clone()));

        let skip = Conflicts::new(OverwriteMode::Skip);
        assert_eq!(skip.resolve(&existing).unwrap(), None);
        assert_eq!(skip.skipped(), 1);

        let rename = Conflicts::new(OverwriteMode::Rename);
        assert_eq!(rename.resolve(&existing).unwrap(), Some(dir.join("a (1).txt")));
        assert_eq!(rename.renamed(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn renaming_skips_taken_and_reserved_names() {
        let dir = scratch("rename");
        for name in ["a.txt", "a (1).txt", "b.txt", "noext"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let conflicts = Conflicts::new(OverwriteMode::Rename);
        assert_eq!(conflicts.resolve(&dir.join("a.txt")).unwrap(), Some(dir.join("a (2).txt")));
        assert_eq!(conflicts.resolve(&dir.join("noext")).unwrap(), Some(dir.join("noext (1)")));
        // Same target again: same answer, not "a (3).txt".
        assert_eq!(conflicts.resolve(&dir.join("a.txt")).unwrap(), Some(dir.join("a (2).txt")));
        assert_eq!(conflicts.renamed(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prompter_is_asked_once_per_file_and_can_abort() {
        let dir = scratch("ask");
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        std::fs::write(dir.join("b.txt"), b"x").unwrap();
        let prompter = Arc::new(Scripted(Mutex::new(vec![Decision::Skip, Decision::Abort]), AtomicUsize::new(0)));
        let conflicts = Conflicts::asking(prompter.clone());

        assert_eq!(conflicts.resolve(&dir.join("a.txt")).unwrap(), None);
        assert_eq!(conflicts.resolve(&dir.join("a.txt")).unwrap(), None); // remembered
        assert_eq!(prompter.1.load(Ordering::Relaxed), 1);

        let err = conflicts.resolve(&dir.join("b.txt")).unwrap_err();
        assert!(err.get_ref().is_some_and(|inner| inner.is::<Aborted>()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_directories_are_not_conflicts() {
        let dir = scratch("dirs");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        assert_eq!(Conflicts::new(OverwriteMode::Skip).resolve(&dir.join("sub")).unwrap(), Some(dir.join("sub")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
