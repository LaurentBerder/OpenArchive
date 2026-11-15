use anyhow::{Context, Result};
use sevenz_rust2::encoder_options::{AesEncoderOptions, EncoderOptions, Lzma2Options};
use sevenz_rust2::{
    ArchiveEntry as SzEntry, ArchiveReader as SzReader, ArchiveWriter as SzWriter,
    EncoderConfiguration, EncoderMethod, Password, PreparedBlock, SourceReader, prepare_block,
};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, Write, copy};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use walkdir::WalkDir;

use crate::core::{ArchiveEntry, ArchiveReader, ArchiveWriter, CompressionOptions, ExtractOptions};

const MB: u64 = 1024 * 1024;

/// Upper bound on the uncompressed size of a block prepared in memory.
/// Larger blocks are streamed through the writer instead, so a single huge
/// file cannot force its entire compressed image into RAM.
const MAX_PREPARED_BLOCK: u64 = 256 * MB;

/// Target maximum size of one solid block when compressing in parallel.
/// Smaller blocks improve load balancing and bound peak memory; larger blocks
/// improve the compression ratio.
const PARALLEL_BLOCK_SIZE: u64 = 64 * MB;

/// Do not split a small archive into tiny parallel blocks; the thread and
/// per-block LZMA2 state overhead would outweigh the gain.
const MIN_PARALLEL_BLOCK_SIZE: u64 = 8 * MB;

/// A block at least this large is worth parallelizing inside its own LZMA2
/// stream when it is not compressed alongside other blocks.
const MT_BLOCK_THRESHOLD: u64 = 8 * MB;

/// Uncompressed chunk size for multi-threaded LZMA2. Must be at least the
/// dictionary size; `Lzma2Options` clamps it.
const MT_CHUNK_SIZE: u64 = 32 * MB;

struct LazyFileReader {
    path: PathBuf,
    reader: Option<BufReader<File>>,
    end: bool,
}

impl LazyFileReader {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            reader: None,
            end: false,
        }
    }
}

impl Read for LazyFileReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.end {
            return Ok(0);
        }
        if self.reader.is_none() {
            let f = File::open(&self.path)?;
            self.reader = Some(BufReader::with_capacity(64 * 1024, f));
        }
        let n = self.reader.as_mut().unwrap().read(buf)?;
        if n == 0 {
            self.end = true;
            self.reader = None; // Release OS file handle immediately
        }
        Ok(n)
    }
}

pub struct SevenZHandler {
    path: PathBuf,
    password: Option<String>,
}

impl SevenZHandler {
    pub fn new<P: AsRef<Path>>(path: P, password: Option<String>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            password,
        }
    }
}

impl ArchiveReader for SevenZHandler {
    fn list(&mut self) -> Result<Vec<ArchiveEntry>> {
        let pass = self
            .password
            .as_deref()
            .map(Password::from)
            .unwrap_or_else(Password::empty);
        let reader = SzReader::open(&self.path, pass)
            .with_context(|| format!("Failed to open 7z archive: {}", self.path.display()))?;

        let mut entries = Vec::new();
        for f in reader.archive().files.iter() {
            entries.push(ArchiveEntry {
                name: f.name().to_string(),
                size: f.size(),
                compressed_size: None, // 7z stores solid block compressed sizes
                is_dir: f.is_directory(),
                modified: None,
                crc32: if f.has_crc { Some(f.crc) } else { None },
                encrypted: false,
            });
        }

        Ok(entries)
    }

    fn extract(&mut self, options: &ExtractOptions, filter: Option<&[String]>) -> Result<()> {
        let pass = options
            .password
            .as_deref()
            .or(self.password.as_deref())
            .map(Password::from)
            .unwrap_or_else(Password::empty);

        let mut reader = SzReader::open(&self.path, pass)
            .with_context(|| format!("Failed to open 7z archive: {}", self.path.display()))?;

        // 1. Directory pre-creation up front: avoid per-file filesystem collision & overhead
        let mut dir_set: HashSet<PathBuf> = HashSet::new();
        dir_set.insert(options.output_dir.clone());

        for f in reader.archive().files.iter() {
            let name = f.name();
            if let Some(filters) = filter {
                if !filters.is_empty() && !filters.iter().any(|fil| name.contains(fil)) {
                    continue;
                }
            }

            let target_path = if options.flat {
                let file_name = Path::new(name).file_name().unwrap_or_default();
                options.output_dir.join(file_name)
            } else {
                options.output_dir.join(name)
            };

            if f.is_directory() {
                if !options.flat {
                    dir_set.insert(target_path);
                }
            } else if let Some(parent) = target_path.parent() {
                dir_set.insert(parent.to_path_buf());
            }
        }

        for dir in &dir_set {
            fs::create_dir_all(dir)?;
        }

        let num_threads = options
            .threads
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
            })
            .max(1);

        if num_threads <= 1 {
            // Direct extraction with buffered I/O
            reader.for_each_entries(|entry, stream| {
                let name = entry.name();

                if let Some(filters) = filter {
                    if !filters.is_empty() && !filters.iter().any(|f| name.contains(f)) {
                        return Ok(true);
                    }
                }

                if !entry.is_directory() {
                    let target_path = if options.flat {
                        let file_name = Path::new(name).file_name().unwrap_or_default();
                        options.output_dir.join(file_name)
                    } else {
                        options.output_dir.join(name)
                    };

                    let out_file = File::create(&target_path)?;
                    let mut buf_out = BufWriter::with_capacity(64 * 1024, out_file);
                    copy(stream, &mut buf_out)?;
                    buf_out.flush()?;
                }

                Ok(true)
            })?;
        } else {
            // Bounded producer/consumer pipeline:
            // Decompressor streams small/medium files into a bounded queue;
            // a worker pool writes them in parallel, decoupling disk I/O from LZMA2 decoding.
            let bound = (num_threads * 4).max(8);
            let (tx, rx) = mpsc::sync_channel::<(PathBuf, Vec<u8>)>(bound);
            let first_err: Mutex<Option<anyhow::Error>> = Mutex::new(None);
            let rx = Arc::new(Mutex::new(rx));

            std::thread::scope(|s| {
                for _ in 0..num_threads {
                    let rx = rx.clone();
                    let first_err = &first_err;
                    s.spawn(move || {
                        loop {
                            if first_err.lock().unwrap().is_some() {
                                break;
                            }
                            let task = {
                                let guard = rx.lock().unwrap();
                                guard.recv().ok()
                            };
                            let Some((target_path, data)) = task else {
                                break;
                            };

                            let res: Result<()> = (|| {
                                let out_file = File::create(&target_path)?;
                                let mut buf_out = BufWriter::with_capacity(64 * 1024, out_file);
                                buf_out.write_all(&data)?;
                                buf_out.flush()?;
                                Ok(())
                            })();

                            if let Err(e) = res {
                                let mut g = first_err.lock().unwrap();
                                if g.is_none() {
                                    *g = Some(e);
                                }
                                break;
                            }
                        }
                    });
                }

                let res = reader.for_each_entries(|entry, stream| {
                    if first_err.lock().unwrap().is_some() {
                        return Ok(false);
                    }

                    let name = entry.name();
                    if let Some(filters) = filter {
                        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f)) {
                            return Ok(true);
                        }
                    }

                    if !entry.is_directory() {
                        let target_path = if options.flat {
                            let file_name = Path::new(name).file_name().unwrap_or_default();
                            options.output_dir.join(file_name)
                        } else {
                            options.output_dir.join(name)
                        };

                        const MAX_ASYNC_SIZE: u64 = 8 * 1024 * 1024; // 8 MB threshold for in-memory pipeline
                        if entry.size() <= MAX_ASYNC_SIZE {
                            let mut data = Vec::with_capacity(entry.size() as usize);
                            stream.read_to_end(&mut data)?;
                            if tx.send((target_path, data)).is_err() {
                                return Ok(false);
                            }
                        } else {
                            // Large files streamed directly with 128KB buffer
                            let out_file = File::create(&target_path)?;
                            let mut buf_out = BufWriter::with_capacity(128 * 1024, out_file);
                            copy(stream, &mut buf_out)?;
                            buf_out.flush()?;
                        }
                    }

                    Ok(true)
                });

                drop(tx);

                if let Err(e) = res {
                    let mut g = first_err.lock().unwrap();
                    if g.is_none() {
                        *g = Some(e.into());
                    }
                }
            });

            if let Some(err) = first_err.into_inner().unwrap() {
                return Err(err);
            }
        }

        Ok(())
    }

    fn test(&mut self, password: Option<&str>) -> Result<()> {
        let pass = password
            .or(self.password.as_deref())
            .map(Password::from)
            .unwrap_or_else(Password::empty);

        let mut reader = SzReader::open(&self.path, pass)
            .with_context(|| format!("Failed to open 7z archive: {}", self.path.display()))?;

        let mut sink = io::sink();
        reader.for_each_entries(|_entry, stream| {
            copy(stream, &mut sink)?;
            Ok(true)
        })?;

        Ok(())
    }
}

/// One file queued for compression, with its already-built 7z entry.
struct FileItem {
    entry: SzEntry,
    path: PathBuf,
    size: u64,
}

type Block = Vec<FileItem>;

/// Size limit for one solid block.
///
/// A single-thread run uses one large block (best ratio). A parallel run aims
/// for roughly one block per thread, clamped so blocks stay small enough to
/// prepare in memory and large enough for LZMA2 to find long-range matches.
fn block_size(num_threads: usize, total_size: u64) -> u64 {
    if num_threads <= 1 {
        return u64::MAX;
    }

    // Escape hatch used by the integration tests to exercise the multi-block
    // path on small fixtures.
    if let Some(limit) = std::env::var("OPENARCHIVE_7Z_BLOCK_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return limit.max(1);
    }

    (total_size / num_threads as u64).clamp(MIN_PARALLEL_BLOCK_SIZE, PARALLEL_BLOCK_SIZE)
}

fn block_uncompressed_size(block: &Block) -> u64 {
    block
        .iter()
        .fold(0u64, |acc, item| acc.saturating_add(item.size))
}

/// Group files into solid blocks. A file is never split across blocks, so an
/// oversized file simply forms a block of its own.
fn build_blocks(items: Vec<FileItem>, solid: bool, num_threads: usize) -> Vec<Block> {
    if !solid {
        return items.into_iter().map(|item| vec![item]).collect();
    }

    let total_size: u64 = items
        .iter()
        .fold(0u64, |acc, item| acc.saturating_add(item.size));
    let limit = block_size(num_threads, total_size);

    let mut blocks: Vec<Block> = Vec::new();
    let mut current: Block = Vec::new();
    let mut current_size = 0u64;

    for item in items {
        if !current.is_empty() && current_size.saturating_add(item.size) > limit {
            blocks.push(std::mem::take(&mut current));
            current_size = 0;
        }
        current_size = current_size.saturating_add(item.size);
        current.push(item);
    }

    if !current.is_empty() {
        blocks.push(current);
    }

    blocks
}

fn lzma2_method(level: u8, dict_size: Option<u32>, threads: usize) -> EncoderConfiguration {
    let mut options = if threads > 1 {
        // The chunk must be at least the dictionary size; `from_level_mt`
        // clamps it before a custom `-md` is applied below, so account for
        // that dictionary here.
        let chunk_size = MT_CHUNK_SIZE.max(dict_size.unwrap_or(0) as u64);
        Lzma2Options::from_level_mt(level as u32, threads as u32, chunk_size)
    } else {
        Lzma2Options::from_level(level as u32)
    };

    if let Some(dict_size) = dict_size {
        options.set_dictionary_size(dict_size);
    }

    EncoderConfiguration::new(EncoderMethod::LZMA2).with_options(EncoderOptions::Lzma2(options))
}

/// Build the coder chain for a block. AES comes first when a password is set,
/// matching the previous single-stream writer.
fn content_methods(
    level: u8,
    dict_size: Option<u32>,
    password: Option<&str>,
    threads: usize,
) -> Vec<EncoderConfiguration> {
    let mut methods = Vec::with_capacity(2);

    if let Some(password) = password.filter(|p| !p.is_empty()) {
        methods.push(EncoderConfiguration::from(AesEncoderOptions::new(
            Password::from(password),
        )));
    }

    // Match 7-Zip: `-mx=0` stores entries instead of running LZMA2 at preset 0.
    if level == 0 {
        methods.push(EncoderConfiguration::new(EncoderMethod::COPY));
    } else {
        methods.push(lzma2_method(level, dict_size, threads));
    }

    methods
}

/// Compress `blocks` on a worker pool and append them in archive order.
///
/// `prepare_block` moves the expensive encoding off the writer thread and
/// returns a self-contained compressed block. A sliding window stops workers
/// from racing more than `worker_count` blocks ahead of the writer, so peak
/// memory is bounded by the number of in-flight blocks rather than the number
/// of blocks in the archive.
fn push_prepared_blocks<W: Write + Seek>(
    writer: &mut SzWriter<W>,
    blocks: Vec<Block>,
    methods: Arc<Vec<EncoderConfiguration>>,
    max_workers: usize,
) -> Result<()> {
    if blocks.is_empty() {
        return Ok(());
    }

    let worker_count = max_workers.min(blocks.len()).max(1);
    let next_idx = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let pushed_gate = Mutex::new(0usize);
    let pushed_cv = Condvar::new();
    let (tx, rx) = mpsc::sync_channel::<(usize, Result<PreparedBlock>)>(worker_count);

    let cancel = || {
        failed.store(true, Ordering::Relaxed);
        // Take the lock so a worker cannot miss the notification between its
        // `failed` check and its `wait`.
        let _guard = pushed_gate.lock().unwrap();
        pushed_cv.notify_all();
    };

    let result: Result<()> = std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let tx = tx.clone();
            let methods = methods.clone();
            let blocks = &blocks;
            let next_idx = &next_idx;
            let failed = &failed;
            let pushed_gate = &pushed_gate;
            let pushed_cv = &pushed_cv;

            scope.spawn(move || {
                loop {
                    if failed.load(Ordering::Relaxed) {
                        return;
                    }

                    let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                    if idx >= blocks.len() {
                        return;
                    }

                    // Window: at most `worker_count` blocks may run ahead of
                    // the block the writer is waiting for.
                    {
                        let mut pushed = pushed_gate.lock().unwrap();
                        while idx >= *pushed + worker_count {
                            if failed.load(Ordering::Relaxed) {
                                return;
                            }
                            pushed = pushed_cv.wait(pushed).unwrap();
                        }
                    }

                    let entries: Vec<SzEntry> =
                        blocks[idx].iter().map(|item| item.entry.clone()).collect();
                    let readers: Vec<SourceReader<LazyFileReader>> = blocks[idx]
                        .iter()
                        .map(|item| SourceReader::new(LazyFileReader::new(item.path.clone())))
                        .collect();

                    let prepared = prepare_block(methods.clone(), entries, readers)
                        .map_err(|e| anyhow::anyhow!("7z block {} compression failed: {}", idx, e));

                    if tx.send((idx, prepared)).is_err() {
                        return;
                    }
                }
            });
        }

        drop(tx);

        let mut pending: BTreeMap<usize, PreparedBlock> = BTreeMap::new();
        let mut pushed = 0usize;

        while pushed < blocks.len() {
            match rx.recv() {
                Ok((idx, Ok(block))) => {
                    pending.insert(idx, block);
                }
                Ok((_idx, Err(e))) => {
                    cancel();
                    return Err(e);
                }
                Err(_) => {
                    cancel();
                    anyhow::bail!(
                        "7z compression pipeline stopped after {} of {} blocks",
                        pushed,
                        blocks.len()
                    );
                }
            }

            while let Some(block) = pending.remove(&pushed) {
                if let Err(e) = writer.push_prepared_block(block) {
                    cancel();
                    return Err(e.into());
                }
                pushed += 1;
                {
                    let mut gate = pushed_gate.lock().unwrap();
                    *gate = pushed;
                }
                pushed_cv.notify_all();
            }
        }

        Ok(())
    });

    result
}

impl ArchiveWriter for SevenZHandler {
    fn create(archive_path: &Path, files: &[PathBuf], options: &CompressionOptions) -> Result<()> {
        let level = options.level.clamp(0, 9);
        let num_threads = options
            .threads
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
            })
            .max(1);

        let single_methods =
            content_methods(level, options.dict_size, options.password.as_deref(), 1);

        let mut writer = SzWriter::create(archive_path)
            .with_context(|| format!("Failed to create 7z archive: {}", archive_path.display()))?;
        writer.set_content_methods(single_methods.clone());

        // Separate directories and file items to support solid block compression
        let mut dir_entries: Vec<SzEntry> = Vec::new();
        let mut file_items: Vec<FileItem> = Vec::new();

        for target in files {
            if target.is_dir() {
                let base = target.parent().unwrap_or(target);
                for entry in WalkDir::new(target) {
                    let entry = entry?;
                    let path = entry.path();
                    let rel_path = path.strip_prefix(base).unwrap_or(path);
                    let rel_name = rel_path.to_string_lossy().replace('\\', "/");

                    if path.is_dir() {
                        dir_entries.push(SzEntry::from_path(path, rel_name));
                    } else if path.is_file() {
                        let size = entry.metadata()?.len();
                        file_items.push(FileItem {
                            entry: SzEntry::from_path(path, rel_name),
                            path: path.to_path_buf(),
                            size,
                        });
                    }
                }
            } else if target.is_file() {
                let file_name = target
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let size = target.metadata()?.len();
                file_items.push(FileItem {
                    entry: SzEntry::from_path(target, file_name),
                    path: target.clone(),
                    size,
                });
            }
        }

        // 1. Add all directory entries first
        for dir_entry in dir_entries {
            writer.push_archive_entry(dir_entry, None::<&[u8]>)?;
        }

        let blocks = build_blocks(file_items, options.solid, num_threads);
        if blocks.is_empty() {
            writer.finish()?;
            return Ok(());
        }

        // A block larger than the in-memory budget is streamed through the
        // writer; mixing prepared and streamed blocks would reorder entries,
        // so the whole archive takes the streaming path in that case.
        let has_oversized_block = blocks
            .iter()
            .any(|b| block_uncompressed_size(b) > MAX_PREPARED_BLOCK);

        if num_threads > 1 && blocks.len() > 1 && !has_oversized_block {
            // Parallel solid blocks: every block is encoded on a worker and
            // appended in order by the writer thread.
            let methods = Arc::new(single_methods);
            push_prepared_blocks(&mut writer, blocks, methods, num_threads)?;
        } else {
            // Streaming path: one block (or many independent blocks in
            // non-solid mode). Large blocks use multi-threaded LZMA2 inside
            // their own stream, which keeps memory bounded.
            for block in blocks {
                let size = block_uncompressed_size(&block);
                let methods = if num_threads > 1 && size >= MT_BLOCK_THRESHOLD {
                    content_methods(
                        level,
                        options.dict_size,
                        options.password.as_deref(),
                        num_threads,
                    )
                } else {
                    single_methods.clone()
                };
                writer.set_content_methods(methods);

                let mut entries = Vec::with_capacity(block.len());
                let mut readers = Vec::with_capacity(block.len());
                for item in block {
                    entries.push(item.entry);
                    readers.push(SourceReader::new(LazyFileReader::new(item.path)));
                }
                writer.push_archive_entries(entries, readers)?;
            }
        }

        writer.finish()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_zero_uses_copy() {
        let methods = content_methods(0, None, None, 4);
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].method, EncoderMethod::COPY);
    }

    #[test]
    fn nonzero_level_uses_lzma2() {
        let methods = content_methods(5, Some(16 * 1024 * 1024), None, 4);
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].method, EncoderMethod::LZMA2);
    }

    #[test]
    fn level_zero_with_password_keeps_aes_then_copy() {
        let methods = content_methods(0, None, Some("secret"), 1);
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].method, EncoderMethod::AES256_SHA256);
        assert_eq!(methods[1].method, EncoderMethod::COPY);
    }
}
