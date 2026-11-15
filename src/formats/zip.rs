use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Seek, Write, copy, sink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, mpsc};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::core::io as file_io;
use crate::core::{ArchiveEntry, ArchiveReader, ArchiveWriter, CompressionOptions, ExtractOptions};

/// Target uncompressed size of one parallel ZIP batch. The compressed batch is
/// held in memory until it is merged, so this value directly bounds per-worker
/// memory.
const DEFAULT_BATCH_BYTES: u64 = 32 * 1024 * 1024;

/// Cap the number of entries in one batch so many tiny files still produce
/// reasonably sized work units without huge local-file tables.
const MAX_BATCH_FILES: usize = 256;

/// Total budget for prepared (not yet merged) batches. Batch size is derived
/// from this budget and the requested thread count.
const MAX_INFLIGHT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone)]
struct ZipItem {
    path: PathBuf,
    name: String,
    size: u64,
}

fn zip_batch_limit(num_threads: usize) -> u64 {
    let requested = std::env::var("OPENARCHIVE_ZIP_BATCH_BYTES")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_BATCH_BYTES)
        .max(1);

    // Split the in-flight budget across the requested workers instead of
    // silently capping worker count. With the default 256 MB / 32 MB this is
    // 8 workers; with 12 threads each batch is ~21 MB and all 12 workers run.
    let per_worker = (MAX_INFLIGHT_BYTES / num_threads.max(1) as u64).max(1);

    requested.min(per_worker).min(MAX_INFLIGHT_BYTES).max(1)
}

fn batch_bytes(batch: &[ZipItem]) -> u64 {
    batch
        .iter()
        .fold(0u64, |acc, item| acc.saturating_add(item.size))
}

/// Group files by uncompressed size and entry count. Entry order is preserved.
/// A single oversized file forms a block of its own; callers that need strict
/// memory bounds stream such files instead of preparing them.
fn build_batches(items: Vec<ZipItem>, max_batch_bytes: u64) -> Vec<Vec<ZipItem>> {
    let mut batches: Vec<Vec<ZipItem>> = Vec::new();
    let mut current: Vec<ZipItem> = Vec::new();
    let mut current_bytes = 0u64;

    for item in items {
        let would_exceed_bytes = current_bytes.saturating_add(item.size) > max_batch_bytes;
        let would_exceed_count = current.len() >= MAX_BATCH_FILES;

        if !current.is_empty() && (would_exceed_bytes || would_exceed_count) {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }

        current_bytes = current_bytes.saturating_add(item.size);
        current.push(item);
    }

    if !current.is_empty() {
        batches.push(current);
    }

    batches
}

pub struct ZipHandler {
    path: PathBuf,
    password: Option<String>,
}

impl ZipHandler {
    pub fn new<P: AsRef<Path>>(path: P, password: Option<String>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            password,
        }
    }
}

impl ArchiveReader for ZipHandler {
    fn list(&mut self) -> Result<Vec<ArchiveEntry>> {
        let file = File::open(&self.path)
            .with_context(|| format!("Failed to open zip archive: {}", self.path.display()))?;
        let mut archive = ZipArchive::new(file)?;

        let mut entries = Vec::new();
        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            entries.push(ArchiveEntry {
                name: file.name().to_string(),
                size: file.size(),
                compressed_size: Some(file.compressed_size()),
                is_dir: file.is_dir(),
                modified: None,
                crc32: Some(file.crc32() as u64),
                encrypted: file.encrypted(),
            });
        }

        Ok(entries)
    }

    fn extract(&mut self, options: &ExtractOptions, filter: Option<&[String]>) -> Result<()> {
        let file = File::open(&self.path)
            .with_context(|| format!("Failed to open zip archive: {}", self.path.display()))?;
        let mut archive = ZipArchive::new(file)?;

        // 1. Collect directories and filtered file entries
        let mut dirs_to_create: HashSet<PathBuf> = HashSet::new();
        dirs_to_create.insert(options.output_dir.clone());
        let mut files_to_extract: Vec<(usize, PathBuf, u64)> = Vec::new();

        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            let name = file.name().to_string();

            if let Some(filters) = filter {
                if !filters.is_empty() && !filters.iter().any(|f| name.contains(f)) {
                    continue;
                }
            }

            let target_path = if options.flat {
                let file_name = Path::new(&name).file_name().unwrap_or_default();
                options.output_dir.join(file_name)
            } else {
                options.output_dir.join(&name)
            };

            if file.is_dir() {
                if !options.flat {
                    dirs_to_create.insert(target_path);
                }
            } else {
                if let Some(parent) = target_path.parent() {
                    dirs_to_create.insert(parent.to_path_buf());
                }
                files_to_extract.push((i, target_path, file.size()));
            }
        }

        // 2. Pre-create all directories up front sequentially
        for dir in &dirs_to_create {
            fs::create_dir_all(dir)?;
        }

        if files_to_extract.is_empty() {
            return Ok(());
        }

        // 3. Determine parallelism
        let num_threads = options
            .threads
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
            })
            .max(1);

        let pass = options.password.as_deref().or(self.password.as_deref());

        if num_threads <= 1 || files_to_extract.len() <= 1 {
            // Sequential path
            for (i, target_path, size) in files_to_extract {
                let mut file = if let Some(p) = pass {
                    archive.by_index_decrypt(i, p.as_bytes())?
                } else {
                    archive.by_index(i)?
                };
                file_io::copy_to_file(&mut file, &target_path, size)?;
            }
        } else {
            // Parallel extraction: each worker thread has an independent archive handle
            let worker_count = num_threads.min(files_to_extract.len());
            let next_idx = AtomicUsize::new(0);
            let first_err: Mutex<Option<anyhow::Error>> = Mutex::new(None);
            let pass = pass.map(|s| s.to_string());
            let archive_path = &self.path;
            let files_ref = &files_to_extract;

            std::thread::scope(|s| {
                for _ in 0..worker_count {
                    s.spawn(|| {
                        let f = match File::open(archive_path) {
                            Ok(f) => f,
                            Err(e) => {
                                let mut g = first_err.lock().unwrap();
                                if g.is_none() {
                                    *g = Some(e.into());
                                }
                                return;
                            }
                        };
                        let mut arch = match ZipArchive::new(f) {
                            Ok(a) => a,
                            Err(e) => {
                                let mut g = first_err.lock().unwrap();
                                if g.is_none() {
                                    *g = Some(e.into());
                                }
                                return;
                            }
                        };

                        loop {
                            if first_err.lock().unwrap().is_some() {
                                break;
                            }
                            let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                            if idx >= files_ref.len() {
                                break;
                            }
                            let (entry_idx, ref target_path, size) = files_ref[idx];
                            let res: Result<()> = (|| {
                                let mut file = if let Some(ref p) = pass {
                                    arch.by_index_decrypt(entry_idx, p.as_bytes())?
                                } else {
                                    arch.by_index(entry_idx)?
                                };
                                file_io::copy_to_file(&mut file, target_path, size)?;
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
            });

            if let Some(err) = first_err.into_inner().unwrap() {
                return Err(err);
            }
        }

        Ok(())
    }

    fn test(&mut self, password: Option<&str>) -> Result<()> {
        let file = File::open(&self.path)
            .with_context(|| format!("Failed to open zip archive: {}", self.path.display()))?;
        let mut archive = ZipArchive::new(file)?;

        let pass = password.or(self.password.as_deref());

        let mut out = sink();
        for i in 0..archive.len() {
            let mut file = if let Some(p) = pass {
                archive.by_index_decrypt(i, p.as_bytes())?
            } else {
                archive.by_index(i)?
            };
            copy(&mut file, &mut out)?;
        }

        Ok(())
    }
}

/// Compress ZIP batches on a worker pool and merge them in archive order.
///
/// Each worker prepares a complete sub-archive in memory. A sliding window
/// prevents workers from running more than `worker_count` batches ahead of the
/// merger, so the reorder buffer can never grow beyond the in-flight batch
/// budget; batches are themselves size-limited by the caller.
fn push_parallel_batches<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    batches: Vec<Vec<ZipItem>>,
    file_options: SimpleFileOptions,
    worker_count: usize,
) -> Result<()> {
    let worker_count = worker_count.min(batches.len()).max(1);
    let next_idx = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let pushed_gate = Mutex::new(0usize);
    let pushed_cv = Condvar::new();
    let (tx, rx) = mpsc::sync_channel::<(usize, Result<Cursor<Vec<u8>>>)>(worker_count);

    let cancel = || {
        failed.store(true, Ordering::Relaxed);
        // Hold the lock so a waiting worker cannot miss the notification.
        let _guard = pushed_gate.lock().unwrap();
        pushed_cv.notify_all();
    };

    let result: Result<()> = std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let tx = tx.clone();
            let batches = &batches;
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
                    if idx >= batches.len() {
                        return;
                    }

                    // Window: at most `worker_count` batches may be prepared
                    // ahead of the batch the merger needs next.
                    {
                        let mut pushed = pushed_gate.lock().unwrap();
                        while idx >= *pushed + worker_count {
                            if failed.load(Ordering::Relaxed) {
                                return;
                            }
                            pushed = pushed_cv.wait(pushed).unwrap();
                        }
                    }

                    let batch = &batches[idx];
                    let res: Result<Cursor<Vec<u8>>> = (|| {
                        // Do not reserve the whole uncompressed size up front:
                        // compressible data needs far less, and the Vec will
                        // grow as needed.
                        let initial_capacity = (batch_bytes(batch) as usize).min(4 * 1024 * 1024);
                        let cursor = Cursor::new(Vec::with_capacity(initial_capacity));
                        let mut sub_writer = ZipWriter::new(cursor);
                        for item in batch {
                            sub_writer.start_file(item.name.clone(), file_options)?;
                            file_io::copy_file_to(&item.path, item.size, &mut sub_writer)?;
                        }
                        Ok(sub_writer.finish()?)
                    })();

                    if tx.send((idx, res)).is_err() {
                        return;
                    }
                }
            });
        }

        drop(tx);

        let mut pending: BTreeMap<usize, Cursor<Vec<u8>>> = BTreeMap::new();
        let mut pushed = 0usize;

        while pushed < batches.len() {
            match rx.recv() {
                Ok((idx, Ok(cursor))) => {
                    pending.insert(idx, cursor);
                }
                Ok((_idx, Err(e))) => {
                    cancel();
                    return Err(e);
                }
                Err(_) => {
                    cancel();
                    anyhow::bail!(
                        "Parallel ZIP compression stopped after {} of {} batches",
                        pushed,
                        batches.len()
                    );
                }
            }

            while let Some(cursor) = pending.remove(&pushed) {
                let merged: Result<()> = (|| {
                    let sub_archive = ZipArchive::new(cursor)?;
                    writer.merge_archive(sub_archive)?;
                    Ok(())
                })();
                if let Err(e) = merged {
                    cancel();
                    return Err(e);
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

impl ArchiveWriter for ZipHandler {
    fn create(archive_path: &Path, files: &[PathBuf], options: &CompressionOptions) -> Result<()> {
        let file = File::create(archive_path)
            .with_context(|| format!("Failed to create zip file: {}", archive_path.display()))?;
        let buffered = BufWriter::with_capacity(file_io::ARCHIVE_BUFFER_SIZE, file);
        let mut writer = ZipWriter::new(buffered);

        let (method, comp_level) = if options.level == 0 {
            (CompressionMethod::Stored, None)
        } else {
            (CompressionMethod::Deflated, Some(options.level as i64))
        };

        let file_options = SimpleFileOptions::default()
            .compression_method(method)
            .compression_level(comp_level);

        let dir_options =
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        let mut dir_entries: Vec<String> = Vec::new();
        let mut file_items: Vec<ZipItem> = Vec::new();

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
                        let dir_name = if rel_name.ends_with('/') {
                            rel_name
                        } else {
                            format!("{}/", rel_name)
                        };
                        dir_entries.push(dir_name);
                    } else if file_type.is_file() {
                        file_items.push(ZipItem {
                            path: path.to_path_buf(),
                            name: rel_name,
                            size: entry.metadata()?.len(),
                        });
                    } else if file_type.is_symlink() {
                        // Preserve path-based handling for symlinked inputs.
                        if path.is_dir() {
                            let dir_name = if rel_name.ends_with('/') {
                                rel_name
                            } else {
                                format!("{}/", rel_name)
                            };
                            dir_entries.push(dir_name);
                        } else if path.is_file() {
                            file_items.push(ZipItem {
                                path: path.to_path_buf(),
                                name: rel_name,
                                size: path.metadata()?.len(),
                            });
                        }
                    }
                }
            } else if target_meta.is_file() {
                let file_name = target
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                file_items.push(ZipItem {
                    path: target.clone(),
                    name: file_name,
                    size: target_meta.len(),
                });
            }
        }

        // Add all directory entries first
        for dir_name in dir_entries {
            writer.add_directory(dir_name, dir_options)?;
        }

        if file_items.is_empty() {
            let mut output = writer.finish()?;
            output.flush()?;
            return Ok(());
        }

        let num_threads = options
            .threads
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
            })
            .max(1);

        let total_size = file_items
            .iter()
            .fold(0u64, |acc, item| acc.saturating_add(item.size));
        let max_batch_bytes = zip_batch_limit(num_threads);
        let has_oversized_file = file_items.iter().any(|item| item.size > max_batch_bytes);
        let use_parallel = num_threads > 1
            && file_items.len() > 1
            && !has_oversized_file
            && total_size > max_batch_bytes;

        if !use_parallel {
            // Sequential streaming path. This also handles archives whose
            // files are too large to buffer as a batch, so memory stays
            // bounded regardless of input size.
            for item in file_items {
                writer.start_file(item.name, file_options)?;
                file_io::copy_file_to(&item.path, item.size, &mut writer)?;
            }
        } else {
            // Parallel bounded producer-consumer pipeline.
            let batches = build_batches(file_items, max_batch_bytes);
            let max_workers_by_memory = (MAX_INFLIGHT_BYTES / max_batch_bytes).max(1) as usize;
            let worker_count = num_threads
                .min(batches.len())
                .min(max_workers_by_memory)
                .max(1);
            push_parallel_batches(&mut writer, batches, file_options, worker_count)?;
        }

        let mut output = writer.finish()?;
        output.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(size: u64) -> ZipItem {
        ZipItem {
            path: PathBuf::new(),
            name: String::new(),
            size,
        }
    }

    #[test]
    fn batches_respect_byte_budget() {
        let batches = build_batches(vec![item(4), item(4), item(4), item(4), item(4)], 10);
        assert_eq!(batches.len(), 3);
        assert!(batches.iter().all(|batch| batch_bytes(batch) <= 10));
    }

    #[test]
    fn oversized_item_gets_its_own_batch() {
        let batches = build_batches(vec![item(5), item(20), item(5)], 10);
        assert_eq!(batches.len(), 3);
        assert_eq!(batch_bytes(&batches[1]), 20);
    }

    #[test]
    fn batches_respect_file_count() {
        let batches = build_batches(vec![item(1); MAX_BATCH_FILES + 5], u64::MAX);
        assert_eq!(batches.len(), 2);
        assert!(batches.iter().all(|batch| batch.len() <= MAX_BATCH_FILES));
    }

    #[test]
    fn zero_sized_files_are_batched() {
        let batches = build_batches(vec![item(0); 10], 1024);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 10);
    }
}
