use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod detect;
pub(crate) mod io;
pub(crate) mod paths;
pub mod progress;

pub use progress::{Cancelled, Progress};

/// Logical CPUs available to this process (at least 1).
pub fn available_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Threads to use: the requested count (`-mmt`), or every available CPU;
/// never 0.
fn resolve_threads(requested: Option<usize>) -> usize {
    requested.unwrap_or_else(available_threads).max(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    SevenZip,
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
}

impl ArchiveFormat {
    pub fn from_path<P: AsRef<Path>>(path: P) -> Option<Self> {
        let path = path.as_ref();
        let file_name = path.file_name()?.to_str()?.to_lowercase();

        if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
            return Some(Self::TarGz);
        }
        if file_name.ends_with(".tar.bz2") || file_name.ends_with(".tbz2") {
            return Some(Self::TarBz2);
        }
        if file_name.ends_with(".tar.xz") || file_name.ends_with(".txz") {
            return Some(Self::TarXz);
        }
        if file_name.ends_with(".7z") {
            return Some(Self::SevenZip);
        }
        if file_name.ends_with(".zip") {
            return Some(Self::Zip);
        }
        if file_name.ends_with(".tar") {
            return Some(Self::Tar);
        }

        // No known extension: look at the content, if the file exists.
        detect::detect_content(path)
    }

    /// The format of an existing archive, for opening it: its content first
    /// (7-Zip does the same, so a 7z named `.zip` still opens), then its name.
    ///
    /// Use [`Self::from_path`] to choose the format of an archive being
    /// created: there the name decides, even if an older file of another
    /// format is in the way.
    pub fn detect<P: AsRef<Path>>(path: P) -> Option<Self> {
        let path = path.as_ref();
        detect::detect_content(path).or_else(|| Self::from_path(path))
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::SevenZip => "7z",
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::TarBz2 => "tar.bz2",
            Self::TarXz => "tar.xz",
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ArchiveEntry {
    pub name: String,
    pub size: u64,
    pub compressed_size: Option<u64>,
    pub is_dir: bool,
    pub modified: Option<DateTime<Utc>>,
    pub crc32: Option<u64>,
    pub encrypted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct CompressionOptions {
    pub level: u8, // 0 (Store) to 9 (Ultra)
    pub password: Option<String>,
    pub solid: bool,
    pub dict_size: Option<u32>, // Dictionary size in bytes (e.g. 16MB = 16 * 1024 * 1024)
    pub threads: Option<usize>, // Thread count (None = automatic / default)
}

impl CompressionOptions {
    /// Threads to compress with: `threads` (`-mmt`), or every CPU.
    pub fn thread_count(&self) -> usize {
        resolve_threads(self.threads)
    }
}

impl Default for CompressionOptions {
    fn default() -> Self {
        Self {
            level: 5, // Normal
            password: None,
            solid: true,
            dict_size: None,
            threads: None,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ExtractOptions {
    pub output_dir: PathBuf,
    pub flat: bool,
    pub password: Option<String>,
    pub overwrite: bool,
    pub threads: Option<usize>,
    /// Reports bytes written and lets the caller cancel. On cancellation the
    /// file being written is removed and extraction fails with [`Cancelled`].
    pub progress: Option<Arc<Progress>>,
}

impl ExtractOptions {
    /// Threads to extract with: `threads` (`-mmt`), or every CPU.
    pub fn thread_count(&self) -> usize {
        resolve_threads(self.threads)
    }
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("."),
            flat: false,
            password: None,
            overwrite: true,
            threads: None,
            progress: None,
        }
    }
}

pub trait ArchiveReader {
    fn list(&mut self) -> anyhow::Result<Vec<ArchiveEntry>>;
    fn extract(
        &mut self,
        options: &ExtractOptions,
        filter: Option<&[String]>,
    ) -> anyhow::Result<()>;
    /// Decompresses every entry and verifies it. `progress`, if given,
    /// receives the bytes verified and can cancel the test.
    fn test(&mut self, password: Option<&str>, progress: Option<&Progress>) -> anyhow::Result<()>;
}

pub trait ArchiveWriter {
    fn create(
        archive_path: &Path,
        files: &[PathBuf],
        options: &CompressionOptions,
    ) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_count_defaults_to_every_cpu_and_is_never_zero() {
        let extract = |threads| ExtractOptions { threads, ..Default::default() };
        assert_eq!(extract(None).thread_count(), available_threads());
        assert_eq!(extract(Some(3)).thread_count(), 3);
        assert_eq!(extract(Some(0)).thread_count(), 1);

        let compress = |threads| CompressionOptions { threads, ..Default::default() };
        assert_eq!(compress(None).thread_count(), available_threads());
        assert_eq!(compress(Some(0)).thread_count(), 1);
    }
}
