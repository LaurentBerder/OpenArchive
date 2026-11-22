use anyhow::{Context, Result};
use bzip2::Compression as BzCompression;
use bzip2::read::MultiBzDecoder;
use bzip2::write::BzEncoder;
use flate2::Compression as GzCompression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write, empty};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tar::Header;
use walkdir::WalkDir;

use super::stream_compress::{self, Codec, ParallelCompressor};
use crate::core::io as file_io;
use crate::core::paths;
use crate::core::{
    ArchiveEntry, ArchiveFormat, ArchiveReader, ArchiveWriter, CompressionOptions, ExtractOptions, Progress,
};

struct TarIndexItem {
    offset: u64,
    size: u64,
    target: PathBuf,
}

/// Concurrent writes of large files tend to be slower on Windows with
/// real-time antivirus scanning; large writes are serialized instead.
const LARGE_TAR_ENTRY_BYTES: u64 = 1024 * 1024;

pub struct TarHandler {
    path: PathBuf,
    format: ArchiveFormat,
}

impl TarHandler {
    pub fn new<P: AsRef<Path>>(path: P, format: ArchiveFormat) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            format,
        }
    }

    fn open_reader(&self) -> Result<Box<dyn Read + Send>> {
        let file = file_io::open_sequential(&self.path)
            .with_context(|| format!("Failed to open archive: {}", self.path.display()))?;
        let buf_file = BufReader::with_capacity(file_io::ARCHIVE_BUFFER_SIZE, file);
        match self.format {
            ArchiveFormat::Tar => Ok(Box::new(buf_file)),
            ArchiveFormat::TarGz => Ok(Box::new(MultiGzDecoder::new(buf_file))),
            ArchiveFormat::TarBz2 => Ok(Box::new(MultiBzDecoder::new(buf_file))),
            _ => anyhow::bail!("Unsupported tar format variant"),
        }
    }

    /// Extract a plain (uncompressed, seekable) TAR by indexing entry offsets
    /// first and then copying each entry from an independent file handle.
    /// Returns false when the archive contains something the parallel path does
    /// not preserve (links, special entries, duplicate names), so the caller can
    /// fall back to sequential extraction.
    fn try_extract_plain_parallel(
        &self,
        options: &ExtractOptions,
        filter: Option<&[String]>,
        num_threads: usize,
    ) -> Result<bool> {
        if self.format != ArchiveFormat::Tar || options.flat {
            return Ok(false);
        }

        let file = File::open(&self.path)
            .with_context(|| format!("Failed to open archive: {}", self.path.display()))?;
        let mut archive = tar::Archive::new(file);

        let mut dirs: HashSet<PathBuf> = HashSet::new();
        dirs.insert(options.output_dir.clone());
        let mut files: Vec<TarIndexItem> = Vec::new();
        let mut targets: HashSet<PathBuf> = HashSet::new();

        for entry in archive.entries_with_seek()? {
            let entry = entry?;
            let name = entry.path()?.to_string_lossy().to_string();

            if !paths::matches_filter(filter, &name) {
                continue;
            }

            let Some(target_path) = paths::entry_target(&options.output_dir, &name, false)? else {
                continue;
            };
            let entry_type = entry.header().entry_type();

            if entry_type.is_dir() {
                dirs.insert(target_path);
            } else if entry_type.is_file() {
                if let Some(parent) = target_path.parent() {
                    if !dirs.contains(parent) {
                        dirs.insert(parent.to_path_buf());
                    }
                }
                if !targets.insert(target_path.clone()) {
                    return Ok(false);
                }
                files.push(TarIndexItem {
                    offset: entry.raw_file_position(),
                    size: entry.size(),
                    target: target_path,
                });
            } else {
                return Ok(false);
            }
        }

        if files.is_empty() {
            for dir in &dirs {
                fs::create_dir_all(dir)?;
            }
            return Ok(true);
        }

        let total_size = files
            .iter()
            .fold(0u64, |acc, item| acc.saturating_add(item.size));
        let average_size = total_size / files.len() as u64;
        // Only the indexed (parallel) path knows the total up front; streamed
        // TAR reports bytes without a percentage.
        if let Some(progress) = &options.progress {
            progress.set_total(total_size);
        }

        for dir in &dirs {
            fs::create_dir_all(dir)?;
        }

        if average_size >= LARGE_TAR_ENTRY_BYTES {
            // Large concurrent writes hurt more than the parallelism helps on
            // Windows/AV. Reuse the already-open handle and copy sequentially
            // instead of reopening and rescanning the archive.
            let mut source = archive.into_inner();
            for item in &files {
                source.seek(SeekFrom::Start(item.offset))?;
                let mut reader = Read::by_ref(&mut source).take(item.size);
                file_io::copy_to_file(&mut reader, &item.target, item.size, options.progress.as_deref())?;
            }
            return Ok(true);
        }

        drop(archive);

        let worker_count = num_threads.min(files.len()).max(1);
        let next_index = AtomicUsize::new(0);
        let first_err: Mutex<Option<anyhow::Error>> = Mutex::new(None);
        let large_write_lock = Mutex::new(());
        let archive_path = &self.path;
        let files_ref = &files;

        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                scope.spawn(|| {
                    let mut source = match File::open(archive_path) {
                        Ok(file) => file,
                        Err(e) => {
                            let mut guard = first_err.lock().unwrap();
                            if guard.is_none() {
                                *guard = Some(e.into());
                            }
                            return;
                        }
                    };

                    loop {
                        if first_err.lock().unwrap().is_some() {
                            break;
                        }

                        let index = next_index.fetch_add(1, Ordering::Relaxed);
                        if index >= files_ref.len() {
                            break;
                        }

                        let item = &files_ref[index];
                        let result: Result<()> = (|| {
                            source.seek(SeekFrom::Start(item.offset))?;
                            // Serialize large writes; small files still run
                            // concurrently across workers.
                            let _large_guard = if item.size >= LARGE_TAR_ENTRY_BYTES {
                                Some(large_write_lock.lock().unwrap())
                            } else {
                                None
                            };
                            let mut reader = Read::by_ref(&mut source).take(item.size);
                            file_io::copy_to_file(&mut reader, &item.target, item.size, options.progress.as_deref())?;
                            Ok(())
                        })();

                        if let Err(e) = result {
                            let mut guard = first_err.lock().unwrap();
                            if guard.is_none() {
                                *guard = Some(e);
                            }
                            break;
                        }
                    }
                });
            }
        });

        if let Some(err) = first_err.into_inner().unwrap() {
            return Err(err);
        }

        Ok(true)
    }
}

impl ArchiveReader for TarHandler {
    fn list(&mut self) -> Result<Vec<ArchiveEntry>> {
        let reader = self.open_reader()?;
        let mut archive = tar::Archive::new(reader);

        let mut entries = Vec::new();
        for entry in archive.entries()? {
            let entry = entry?;
            let path_str = entry.path()?.to_string_lossy().to_string();
            let is_dir = entry.header().entry_type().is_dir();
            let size = entry.size();

            entries.push(ArchiveEntry {
                name: path_str,
                size,
                compressed_size: None,
                is_dir,
                modified: None,
                crc32: None,
                encrypted: false,
            });
        }

        Ok(entries)
    }

    fn extract(&mut self, options: &ExtractOptions, filter: Option<&[String]>) -> Result<()> {
        let num_threads = options
            .threads
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
            })
            .max(1);

        if num_threads > 1 && self.try_extract_plain_parallel(options, filter, num_threads)? {
            return Ok(());
        }

        let mut reader = self.open_reader()?;
        if num_threads > 1 && self.format != ArchiveFormat::Tar {
            // Decompress on a separate thread so decoding overlaps with
            // header parsing and file writes.
            reader = Box::new(file_io::ReadAhead::new(reader)?);
        }
        let mut archive = tar::Archive::new(reader);

        fs::create_dir_all(&options.output_dir)?;

        let mut known_dirs: HashSet<PathBuf> = HashSet::new();
        known_dirs.insert(options.output_dir.clone());

        for entry in archive.entries()? {
            let mut entry = entry?;
            let name = entry.path()?.to_string_lossy().to_string();

            if !paths::matches_filter(filter, &name) {
                continue;
            }

            let Some(target_path) = paths::entry_target(&options.output_dir, &name, options.flat)?
            else {
                continue;
            };

            if entry.header().entry_type().is_dir() {
                if !options.flat && known_dirs.insert(target_path.clone()) {
                    fs::create_dir_all(&target_path)?;
                }
            } else {
                if let Some(parent) = target_path.parent() {
                    if !known_dirs.contains(parent) {
                        fs::create_dir_all(parent)?;
                        known_dirs.insert(parent.to_path_buf());
                    }
                }
                let size = entry.size();
                file_io::copy_to_file(&mut entry, &target_path, size, options.progress.as_deref())?;
            }
        }

        Ok(())
    }

    fn test(&mut self, _password: Option<&str>, progress: Option<&Progress>) -> Result<()> {
        let reader = self.open_reader()?;
        let mut archive = tar::Archive::new(reader);

        // Streamed: the total is unknown, so only bytes are reported.
        for entry in archive.entries()? {
            let mut entry = entry?;
            file_io::drain(&mut entry, progress)?;
        }

        Ok(())
    }
}

impl ArchiveWriter for TarHandler {
    fn create(archive_path: &Path, files: &[PathBuf], options: &CompressionOptions) -> Result<()> {
        let file = File::create(archive_path)
            .with_context(|| format!("Failed to create archive: {}", archive_path.display()))?;
        let buf_file = BufWriter::with_capacity(file_io::ARCHIVE_BUFFER_SIZE, file);

        let format = ArchiveFormat::from_path(archive_path).unwrap_or(ArchiveFormat::Tar);
        let writer = TarOutput::new(buf_file, format, options)?;

        let mut builder = tar::Builder::new(writer);

        for target in files {
            let Ok(target_meta) = target.metadata() else {
                continue;
            };

            if target_meta.is_dir() {
                let base = target.parent().unwrap_or(target);
                for entry in WalkDir::new(target) {
                    let entry = entry?;
                    let path = entry.path();
                    let rel_path = path.strip_prefix(base).unwrap_or(path);
                    let rel_name = rel_path.to_string_lossy().replace('\\', "/");
                    let file_type = entry.file_type();

                    if file_type.is_dir() {
                        let metadata = entry.metadata()?;
                        append_tar_dir(&mut builder, &rel_name, &metadata)?;
                    } else if file_type.is_file() {
                        append_tar_file(&mut builder, &rel_name, path)?;
                    } else if file_type.is_symlink() {
                        if path.is_dir() {
                            let metadata = path.metadata()?;
                            append_tar_dir(&mut builder, &rel_name, &metadata)?;
                        } else if path.is_file() {
                            append_tar_file(&mut builder, &rel_name, path)?;
                        }
                    }
                }
            } else if target_meta.is_file() {
                let file_name = target
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                append_tar_file(&mut builder, &file_name, target)?;
            }
        }

        builder.into_inner()?.finish()?;
        Ok(())
    }
}

type ArchiveFile = BufWriter<File>;

/// The byte sink behind `tar::Builder`: the archive file itself, a
/// single-stream encoder, or the block-parallel compressor.
enum TarOutput {
    Plain(ArchiveFile),
    Gzip(GzEncoder<ArchiveFile>),
    Bzip2(BzEncoder<ArchiveFile>),
    Parallel(ParallelCompressor<ArchiveFile>),
}

impl TarOutput {
    fn new(file: ArchiveFile, format: ArchiveFormat, options: &CompressionOptions) -> Result<Self> {
        let codec = match format {
            ArchiveFormat::Tar => return Ok(Self::Plain(file)),
            ArchiveFormat::TarGz => Codec::Gzip(options.level.clamp(0, 9) as u32),
            ArchiveFormat::TarBz2 => Codec::Bzip2(options.level.clamp(1, 9) as u32),
            _ => anyhow::bail!("Invalid tar archive target"),
        };

        let requested = options.threads.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        });
        let workers = stream_compress::worker_count(codec, requested);
        if workers > 1 {
            return Ok(Self::Parallel(ParallelCompressor::new(file, codec, workers)?));
        }

        // One thread: a single gzip member / bzip2 stream, exactly as before.
        Ok(match codec {
            Codec::Gzip(level) => Self::Gzip(GzEncoder::new(file, GzCompression::new(level))),
            Codec::Bzip2(level) => Self::Bzip2(BzEncoder::new(file, BzCompression::new(level))),
        })
    }

    /// Write the compressed stream's trailer and flush the archive file.
    /// Dropping an encoder instead would swallow errors from the final write.
    fn finish(self) -> std::io::Result<()> {
        let mut file = match self {
            Self::Plain(file) => file,
            Self::Gzip(enc) => enc.finish()?,
            Self::Bzip2(enc) => enc.finish()?,
            Self::Parallel(enc) => enc.finish()?,
        };
        file.flush()
    }
}

impl Write for TarOutput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Gzip(w) => w.write(buf),
            Self::Bzip2(w) => w.write(buf),
            Self::Parallel(w) => w.write(buf),
        }
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Plain(w) => w.write_all(buf),
            Self::Gzip(w) => w.write_all(buf),
            Self::Bzip2(w) => w.write_all(buf),
            Self::Parallel(w) => w.write_all(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Gzip(w) => w.flush(),
            Self::Bzip2(w) => w.flush(),
            Self::Parallel(w) => w.flush(),
        }
    }
}

fn append_tar_file<W: Write>(builder: &mut tar::Builder<W>, name: &str, path: &Path) -> Result<()> {
    let file = file_io::open_sequential(path)?;
    // Read metadata from the opened handle so the header size matches the
    // bytes about to be copied, even if the file changed after traversal.
    let metadata = file.metadata()?;

    let mut header = Header::new_gnu();
    header.set_metadata(&metadata);

    if metadata.len() < file_io::SMALL_FILE_THRESHOLD {
        builder.append_data(&mut header, name, file)?;
    } else {
        let mut reader = BufReader::with_capacity(file_io::IO_BUFFER_SIZE, file);
        builder.append_data(&mut header, name, &mut reader)?;
    }

    Ok(())
}

fn append_tar_dir<W: Write>(
    builder: &mut tar::Builder<W>,
    name: &str,
    metadata: &fs::Metadata,
) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_metadata(metadata);
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    builder.append_data(&mut header, name, empty())?;
    Ok(())
}
