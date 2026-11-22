use anyhow::{Context, Result};
use lzma_rust2::{DICT_SIZE_MAX, DICT_SIZE_MIN, LzmaOptions};
use sevenz_rust2::encoder_options::{AesEncoderOptions, EncoderOptions, Lzma2Options};
use sevenz_rust2::{
    ArchiveEntry as SzEntry, ArchiveReader as SzReader, ArchiveWriter as SzWriter, BlockDecoder,
    EncoderConfiguration, EncoderMethod, Password, PreparedBlock, SourceReader, prepare_block,
};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};

use crate::core::io as file_io;
use crate::core::filter::{self, EntryFilter};
use crate::core::inputs::{self, Input};
use crate::core::paths;
use crate::core::{ArchiveEntry, ArchiveReader, ArchiveWriter, CompressionOptions, ExtractOptions, Progress};

const MB: u64 = 1024 * 1024;

/// Upper bound on the uncompressed size of a block prepared in memory.
/// Larger blocks are streamed through the writer instead, so a single huge
/// file cannot force its entire compressed image into RAM.
const MAX_PREPARED_BLOCK: u64 = 256 * MB;

/// Budget for prepared blocks held across all workers. Block size and worker
/// count are both derived from this so a large archive cannot multiply the
/// per-block cap by the thread count.
const MAX_INFLIGHT_BYTES: u64 = 256 * MB;

/// Budget for LZMA2 encoder structures across workers. This is separate from
/// the prepared-output budget: an encoder's dictionary and match finder can be
/// much larger than the compressed block it produces.
const ENCODER_MEMORY_BUDGET_KIB: u64 = 1536 * 1024;

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

/// Maximum bytes buffered by the 7z extraction queue. Limits the producer
/// ahead of the writer pool regardless of thread count or item size.
const MAX_QUEUED_BYTES: usize = 64 * 1024 * 1024;

/// Entries at or below this size are buffered for the single writer thread in
/// the parallel block-decoding path. Larger entries stream directly.
const MAX_BUFFERED_ENTRY: u64 = 32 * MB;

struct ByteBudget {
    capacity: usize,
    used: Mutex<usize>,
    cv: Condvar,
}

impl ByteBudget {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            used: Mutex::new(0),
            cv: Condvar::new(),
        }
    }

    /// Reserve `bytes` from the budget, waiting while the budget is full.
    /// Returns false if the first worker error was recorded in the meantime.
    fn reserve(&self, bytes: usize, first_err: &Mutex<Option<anyhow::Error>>) -> bool {
        let mut used = self.used.lock().unwrap();
        while (*used).saturating_add(bytes) > self.capacity && *used > 0 {
            if first_err.lock().unwrap().is_some() {
                return false;
            }
            used = self.cv.wait(used).unwrap();
        }
        *used = (*used).saturating_add(bytes);
        true
    }

    fn release(&self, bytes: usize) {
        {
            let mut used = self.used.lock().unwrap();
            *used = used.saturating_sub(bytes);
        }
        self.cv.notify_all();
    }

    fn wake_all(&self) {
        let _guard = self.used.lock().unwrap();
        self.cv.notify_all();
    }
}

struct LazyFileReader {
    path: PathBuf,
    buffer_capacity: usize,
    reader: Option<BufReader<File>>,
    end: bool,
}

impl LazyFileReader {
    fn new(path: PathBuf, size: u64) -> Self {
        Self {
            path,
            buffer_capacity: file_io::read_buffer_capacity(size),
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
            let f = file_io::open_sequential(&self.path)?;
            self.reader = Some(BufReader::with_capacity(self.buffer_capacity, f));
        }
        let n = self.reader.as_mut().unwrap().read(buf)?;
        if n == 0 {
            self.end = true;
            self.reader = None; // Release the OS file handle immediately
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

impl SevenZHandler {
    fn list_entries(&mut self) -> Result<Vec<ArchiveEntry>> {
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

    fn extract_entries(&mut self, options: &ExtractOptions, filter: Option<&EntryFilter>) -> Result<()> {
        let pass = options
            .password
            .as_deref()
            .or(self.password.as_deref())
            .map(Password::from)
            .unwrap_or_else(Password::empty);

        let mut reader = SzReader::open(&self.path, pass.clone())
            .with_context(|| format!("Failed to open 7z archive: {}", self.path.display()))?;

        // 1. Directory pre-creation up front: avoid per-file filesystem collision & overhead
        let mut dir_set: HashSet<PathBuf> = HashSet::new();
        dir_set.insert(options.output_dir.clone());
        let mut total_bytes = 0u64;

        for f in reader.archive().files.iter() {
            let name = f.name();
            if !filter::selected(filter, name) {
                continue;
            }

            // This pass sees every name first, so an unsafe one refuses the
            // archive before anything is written.
            let Some(target_path) = paths::entry_target(&options.output_dir, name, options.flat)?
            else {
                continue;
            };

            if f.is_directory() {
                if !options.flat {
                    dir_set.insert(target_path);
                }
            } else {
                // Existing file: overwrite, skip, rename or ask (OA-9). The
                // answer is remembered for the extraction passes below.
                let Some(target_path) = options.conflicts.resolve(&target_path)? else {
                    continue;
                };
                total_bytes = total_bytes.saturating_add(f.size());
                if let Some(parent) = target_path.parent() {
                    if !dir_set.contains(parent) {
                        dir_set.insert(parent.to_path_buf());
                    }
                }
            }
        }

        if let Some(progress) = &options.progress {
            progress.set_total(total_bytes);
        }

        for dir in &dir_set {
            fs::create_dir_all(dir)?;
        }

        let num_threads = options.thread_count();

        let block_count = reader.archive().blocks.len();
        if num_threads > 1 && block_count > 1 && !options.flat {
            // Independent blocks can be decoded in parallel. Flat extraction
            // stays on the ordered path because duplicate basenames would race.
            drop(reader);
            return extract_blocks_parallel(
                &self.path,
                &pass,
                block_count,
                num_threads,
                options,
                filter,
            );
        }

        if num_threads <= 1 {
            // Direct extraction with buffered I/O
            reader.for_each_entries(|entry, stream| {
                let name = entry.name();

                if !filter::selected(filter, name) {
                    return Ok(true);
                }

                if !entry.is_directory() {
                    let Some(target_path) =
                        paths::entry_target(&options.output_dir, name, options.flat)?
                    else {
                        return Ok(true);
                    };
                    let Some(target_path) = options.conflicts.resolve(&target_path)? else {
                        return Ok(true);
                    };

                    file_io::copy_to_file(stream, &target_path, entry.size(), options.progress.as_deref())?;
                }

                Ok(true)
            })?;
        } else {
            // Bounded producer/consumer pipeline:
            // Decompressor streams small/medium files into a bounded queue;
            // a worker pool writes them in parallel, decoupling disk I/O from LZMA2 decoding.
            let bound = (num_threads * 4).max(8);
            let (tx, rx) = mpsc::sync_channel::<(PathBuf, Vec<u8>, usize)>(bound);
            let first_err: Mutex<Option<anyhow::Error>> = Mutex::new(None);
            let rx = Arc::new(Mutex::new(rx));
            let budget = Arc::new(ByteBudget::new(MAX_QUEUED_BYTES));

            std::thread::scope(|s| {
                for _ in 0..num_threads {
                    let rx = rx.clone();
                    let first_err = &first_err;
                    let budget = budget.clone();
                    s.spawn(move || {
                        loop {
                            if first_err.lock().unwrap().is_some() {
                                break;
                            }
                            let task = {
                                let guard = rx.lock().unwrap();
                                guard.recv().ok()
                            };
                            let Some((target_path, data, reserved)) = task else {
                                break;
                            };

                            let res: Result<()> =
                                file_io::write_buffer(&target_path, &data, options.progress.as_deref()).map_err(Into::into);

                            if let Err(e) = res {
                                {
                                    let mut g = first_err.lock().unwrap();
                                    if g.is_none() {
                                        *g = Some(e);
                                    }
                                }
                                budget.release(reserved);
                                budget.wake_all();
                                break;
                            }
                            budget.release(reserved);
                        }
                    });
                }

                let res = reader.for_each_entries(|entry, stream| {
                    if first_err.lock().unwrap().is_some() {
                        return Ok(false);
                    }

                    let name = entry.name();
                    if !filter::selected(filter, name) {
                        return Ok(true);
                    }

                    if !entry.is_directory() {
                        let Some(target_path) =
                            paths::entry_target(&options.output_dir, name, options.flat)?
                        else {
                            return Ok(true);
                        };
                        let Some(target_path) = options.conflicts.resolve(&target_path)? else {
                            return Ok(true);
                        };

                        const MAX_ASYNC_SIZE: u64 = 8 * 1024 * 1024; // 8 MB threshold for in-memory pipeline
                        if entry.size() <= MAX_ASYNC_SIZE {
                            let reserved = entry.size() as usize;
                            if !budget.reserve(reserved, &first_err) {
                                return Ok(false);
                            }

                            let mut data = Vec::with_capacity(reserved);
                            if let Err(e) = stream.read_to_end(&mut data) {
                                budget.release(reserved);
                                return Err(e.into());
                            }

                            let mut item = (target_path, data, reserved);
                            loop {
                                if first_err.lock().unwrap().is_some() {
                                    budget.release(item.2);
                                    return Ok(false);
                                }
                                match tx.try_send(item) {
                                    Ok(()) => break,
                                    Err(mpsc::TrySendError::Full(returned)) => {
                                        item = returned;
                                        std::thread::yield_now();
                                    }
                                    Err(mpsc::TrySendError::Disconnected(returned)) => {
                                        budget.release(returned.2);
                                        return Ok(false);
                                    }
                                }
                            }
                        } else {
                            // Large files streamed directly; small ones were
                            // handled by the bounded pipeline above.
                            file_io::copy_to_file(stream, &target_path, entry.size(), options.progress.as_deref())?;
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

    fn test_entries(&mut self, password: Option<&str>, filter: Option<&EntryFilter>, progress: Option<&Progress>) -> Result<()> {
        let pass = password
            .or(self.password.as_deref())
            .map(Password::from)
            .unwrap_or_else(Password::empty);

        let mut reader = SzReader::open(&self.path, pass)
            .with_context(|| format!("Failed to open 7z archive: {}", self.path.display()))?;

        if let Some(progress) = progress {
            let total = reader
                .archive()
                .files
                .iter()
                .filter(|f| filter::selected(filter, f.name()))
                .fold(0u64, |acc, f| acc.saturating_add(f.size()));
            progress.set_total(total);
        }
        reader.for_each_entries(|entry, stream| {
            if filter::selected(filter, entry.name()) {
                file_io::drain(stream, progress)?;
            }
            Ok(true)
        })?;

        Ok(())
    }
}

impl ArchiveReader for SevenZHandler {
    fn list(&mut self) -> Result<Vec<ArchiveEntry>> {
        self.list_entries()
    }

    fn extract(&mut self, options: &ExtractOptions, filter: Option<&EntryFilter>) -> Result<()> {
        // sevenz-rust2 wraps errors from our own file writes (as
        // MaybeBadPassword when encrypted); report them plainly (OA-27).
        self.extract_entries(options, filter).map_err(super::surface_write_error)
    }

    fn test(&mut self, password: Option<&str>, filter: Option<&EntryFilter>, progress: Option<&Progress>) -> Result<()> {
        self.test_entries(password, filter, progress)
    }
}

/// Decode independent 7z blocks concurrently while serializing file writes.
///
/// Decoding a block is CPU-bound and scales well across blocks, but writing
/// many files concurrently is slower on Windows with real-time antivirus
/// scanning. Decode workers therefore buffer complete entries and a single
/// writer thread performs the disk writes; memory is bounded by the same byte
/// budget used by the regular extraction pipeline.
fn extract_blocks_parallel(
    archive_path: &Path,
    password: &Password,
    block_count: usize,
    num_threads: usize,
    options: &ExtractOptions,
    filter: Option<&EntryFilter>,
) -> Result<()> {
    let archive = sevenz_rust2::Archive::open_with_password(archive_path, password)
        .with_context(|| format!("Failed to open 7z archive: {}", archive_path.display()))?;

    // One thread is reserved for the writer so disk writes do not contend.
    let decode_workers = num_threads.saturating_sub(1).max(1).min(block_count);
    let next_block = AtomicUsize::new(0);
    let first_err: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let budget = Arc::new(ByteBudget::new(MAX_QUEUED_BYTES));
    let write_lock = Mutex::new(());
    let (tx, rx) = mpsc::sync_channel::<(PathBuf, Vec<u8>, usize)>(4);

    std::thread::scope(|scope| {
        // Single writer thread.
        {
            let first_err = &first_err;
            let budget = budget.clone();
            let write_lock = &write_lock;
            scope.spawn(move || {
                while let Ok((target_path, data, reserved)) = rx.recv() {
                    if first_err.lock().unwrap().is_some() {
                        budget.release(reserved);
                        break;
                    }

                    let result = {
                        let _guard = write_lock.lock().unwrap();
                        file_io::write_buffer(&target_path, &data, options.progress.as_deref())
                    };
                    budget.release(reserved);

                    if let Err(e) = result {
                        {
                            let mut guard = first_err.lock().unwrap();
                            if guard.is_none() {
                                *guard = Some(e.into());
                            }
                        }
                        budget.wake_all();
                        break;
                    }
                }
            });
        }

        for _ in 0..decode_workers {
            let archive = &archive;
            let first_err = &first_err;
            let next_block = &next_block;
            let tx = tx.clone();
            let budget = budget.clone();
            let write_lock = &write_lock;

            scope.spawn(move || {
                let mut source = match File::open(archive_path) {
                    Ok(file) => file,
                    Err(e) => {
                        let mut guard = first_err.lock().unwrap();
                        if guard.is_none() {
                            *guard = Some(e.into());
                        }
                        budget.wake_all();
                        return;
                    }
                };

                loop {
                    if first_err.lock().unwrap().is_some() {
                        break;
                    }

                    let block_index = next_block.fetch_add(1, Ordering::Relaxed);
                    if block_index >= block_count {
                        break;
                    }

                    let decoder = BlockDecoder::new(1, block_index, archive, password, &mut source);
                    let result = decoder.for_each_entries(&mut |entry, stream| {
                        if first_err.lock().unwrap().is_some() {
                            return Ok(false);
                        }

                        let name = entry.name();
                        if !filter::selected(filter, name) {
                            return Ok(true);
                        }

                        if !entry.is_directory() {
                            let Some(target_path) =
                                paths::entry_target(&options.output_dir, name, options.flat)?
                            else {
                                return Ok(true);
                            };
                            let Some(target_path) = options.conflicts.resolve(&target_path)? else {
                                return Ok(true);
                            };

                            if entry.size() <= MAX_BUFFERED_ENTRY {
                                let reserved = entry.size() as usize;
                                if !budget.reserve(reserved, &first_err) {
                                    return Ok(false);
                                }

                                let mut data = Vec::with_capacity(reserved);
                                if let Err(e) = stream.read_to_end(&mut data) {
                                    budget.release(reserved);
                                    return Err(e.into());
                                }

                                if tx.send((target_path, data, reserved)).is_err() {
                                    budget.release(reserved);
                                    return Ok(false);
                                }
                            } else {
                                // Very large entries are streamed under the
                                // same write lock so they do not race the
                                // writer thread.
                                let _guard = write_lock.lock().unwrap();
                                if first_err.lock().unwrap().is_some() {
                                    return Ok(false);
                                }
                                file_io::copy_to_file(stream, &target_path, entry.size(), options.progress.as_deref())?;
                            }
                        }

                        Ok(true)
                    });

                    if let Err(e) = result {
                        {
                            let mut guard = first_err.lock().unwrap();
                            if guard.is_none() {
                                *guard = Some(e.into());
                            }
                        }
                        budget.wake_all();
                        break;
                    }
                }
            });
        }

        drop(tx);
    });

    if let Some(err) = first_err.into_inner().unwrap() {
        return Err(err);
    }

    Ok(())
}

/// One file queued for compression, with its already-built 7z entry.
#[derive(Clone)]
struct FileItem {
    entry: SzEntry,
    path: PathBuf,
    size: u64,
}

type Block = Vec<FileItem>;

/// Size limit for one solid block.
///
/// A single-thread run uses one large block (best ratio). A parallel run aims
/// for roughly one block per thread, clamped so the sum of in-flight blocks
/// stays within `MAX_INFLIGHT_BYTES`.
fn block_size(num_threads: usize, total_size: u64) -> u64 {
    if num_threads <= 1 {
        return u64::MAX;
    }

    let per_worker_cap = (MAX_INFLIGHT_BYTES / num_threads as u64).max(1);

    // Escape hatch used by the integration tests to exercise the multi-block
    // path on small fixtures.
    if let Some(limit) = std::env::var("OPENARCHIVE_7Z_BLOCK_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return limit.max(1).min(per_worker_cap);
    }

    let target =
        (total_size / num_threads as u64).clamp(MIN_PARALLEL_BLOCK_SIZE, PARALLEL_BLOCK_SIZE);

    target.min(per_worker_cap).max(1)
}

fn block_uncompressed_size(block: &Block) -> u64 {
    block
        .iter()
        .fold(0u64, |acc, item| acc.saturating_add(item.size))
}

/// Workers allowed to hold prepared blocks, constrained by the total in-flight
/// budget and the largest block (which may be a single oversized file).
fn prepared_worker_count(blocks: &[Block], num_threads: usize) -> usize {
    let largest = blocks
        .iter()
        .map(block_uncompressed_size)
        .max()
        .unwrap_or(0)
        .max(1);
    let by_memory = (MAX_INFLIGHT_BYTES / largest).max(1) as usize;

    num_threads.min(blocks.len()).min(by_memory).max(1)
}

fn lzma2_encoder_memory_kib(level: u8, dict_size: Option<u32>) -> u64 {
    if level == 0 {
        return 0; // COPY: no LZMA2 encoder state.
    }

    let mut options = LzmaOptions::with_preset(level as u32);
    if let Some(dict_size) = dict_size {
        options.dict_size = dict_size.clamp(DICT_SIZE_MIN, DICT_SIZE_MAX);
    }
    options.get_memory_usage() as u64
}

/// Maximum simultaneous LZMA2 encoders allowed by the encoder-memory budget.
fn encoder_worker_cap(level: u8, dict_size: Option<u32>) -> usize {
    let per_worker = lzma2_encoder_memory_kib(level, dict_size);
    if per_worker == 0 {
        return usize::MAX;
    }

    (ENCODER_MEMORY_BUDGET_KIB / per_worker).max(1) as usize
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
                        .map(|item| {
                            SourceReader::new(LazyFileReader::new(item.path.clone(), item.size))
                        })
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
    fn create(archive_path: &Path, files: &[Input], options: &CompressionOptions) -> Result<()> {
        let level = options.level.clamp(0, 9);
        let num_threads = options.thread_count();

        // Dictionary and match-finder memory can dominate a worker's footprint.
        // Scale the effective thread count before choosing block sizes so high
        // levels do not multiply codec memory by the requested thread count.
        let encoder_workers = encoder_worker_cap(level, options.dict_size);
        let effective_threads = num_threads.min(encoder_workers).max(1);

        let single_methods =
            content_methods(level, options.dict_size, options.password.as_deref(), 1);

        let file = File::create(archive_path)
            .with_context(|| format!("Failed to create 7z archive: {}", archive_path.display()))?;
        let buffered = BufWriter::with_capacity(file_io::ARCHIVE_BUFFER_SIZE, file);
        let mut writer = SzWriter::new(buffered)
            .with_context(|| format!("Failed to create 7z archive: {}", archive_path.display()))?;
        writer.set_content_methods(single_methods.clone());

        // Separate directories and file items to support solid block compression
        let mut dir_entries: Vec<SzEntry> = Vec::new();
        let mut file_items: Vec<FileItem> = Vec::new();

        for source in inputs::sources(files)? {
            let entry = SzEntry::from_path(&source.path, source.name);
            if source.is_dir {
                dir_entries.push(entry);
            } else {
                file_items.push(FileItem { entry, path: source.path, size: source.metadata.len() });
            }
        }

        // 1. Add all directory entries first
        for dir_entry in dir_entries {
            writer.push_archive_entry(dir_entry, None::<&[u8]>)?;
        }

        // Block sizing follows the requested thread count so capping workers
        // does not silently produce much larger (and more memory-hungry)
        // blocks; the worker cap only reduces concurrency.
        let blocks = build_blocks(file_items, options.solid, num_threads);
        if blocks.is_empty() {
            let mut output = writer.finish()?;
            output.flush()?;
            return Ok(());
        }

        // A block larger than the in-memory budget is streamed through the
        // writer; mixing prepared and streamed blocks would reorder entries,
        // so the whole archive takes the streaming path in that case.
        let has_oversized_block = blocks
            .iter()
            .any(|b| block_uncompressed_size(b) > MAX_PREPARED_BLOCK);
        let prepared_workers = prepared_worker_count(&blocks, effective_threads);

        if effective_threads > 1 && blocks.len() > 1 && prepared_workers > 1 && !has_oversized_block
        {
            // Parallel solid blocks: every block is encoded on a worker and
            // appended in order by the writer thread.
            let methods = Arc::new(single_methods);
            push_prepared_blocks(&mut writer, blocks, methods, prepared_workers)?;
        } else {
            // Streaming path: one block (or many independent blocks in
            // non-solid mode). Large blocks use multi-threaded LZMA2 inside
            // their own stream, which keeps memory bounded.
            for block in blocks {
                let size = block_uncompressed_size(&block);
                let methods = if effective_threads > 1 && size >= MT_BLOCK_THRESHOLD {
                    content_methods(
                        level,
                        options.dict_size,
                        options.password.as_deref(),
                        effective_threads,
                    )
                } else {
                    single_methods.clone()
                };
                writer.set_content_methods(methods);

                let mut entries = Vec::with_capacity(block.len());
                let mut readers = Vec::with_capacity(block.len());
                for item in block {
                    entries.push(item.entry);
                    readers.push(SourceReader::new(LazyFileReader::new(item.path, item.size)));
                }
                writer.push_archive_entries(entries, readers)?;
            }
        }

        let mut output = writer.finish()?;
        output.flush()?;
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

    fn block_of_size(size: u64) -> Block {
        vec![FileItem {
            entry: SzEntry::default(),
            path: PathBuf::new(),
            size,
        }]
    }

    #[test]
    fn prepared_workers_respect_inflight_budget() {
        let small_blocks = vec![block_of_size(8 * MB); 12];
        assert_eq!(prepared_worker_count(&small_blocks, 12), 12);

        let large_blocks = vec![block_of_size(64 * MB); 12];
        assert_eq!(prepared_worker_count(&large_blocks, 12), 4);
    }

    #[test]
    fn byte_budget_tracks_reservations() {
        let budget = ByteBudget::new(10);
        let error = Mutex::new(None);
        assert!(budget.reserve(6, &error));
        assert!(budget.reserve(4, &error));
        budget.release(6);
        assert!(budget.reserve(6, &error));
    }

    #[test]
    fn encoder_memory_cap_scales_with_level_and_dictionary() {
        assert_eq!(encoder_worker_cap(0, None), usize::MAX);
        assert!(encoder_worker_cap(5, None) >= 8);
        assert!(encoder_worker_cap(9, None) <= 3);
        assert!(lzma2_encoder_memory_kib(9, None) > lzma2_encoder_memory_kib(5, None));
        assert!(
            encoder_worker_cap(5, Some(64 * 1024 * 1024))
                < encoder_worker_cap(5, Some(16 * 1024 * 1024))
        );
    }
}
