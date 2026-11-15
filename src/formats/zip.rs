use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{copy, sink, BufReader, BufWriter, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use anyhow::{Context, Result};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::core::{ArchiveEntry, ArchiveReader, ArchiveWriter, CompressionOptions, ExtractOptions};

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
        let mut files_to_extract: Vec<(usize, PathBuf)> = Vec::new();

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
                files_to_extract.push((i, target_path));
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
        let num_threads = options.threads.unwrap_or_else(|| {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
        }).max(1);

        let pass = options.password.as_deref().or(self.password.as_deref());

        if num_threads <= 1 || files_to_extract.len() <= 1 {
            // Sequential path
            for (i, target_path) in files_to_extract {
                let mut file = if let Some(p) = pass {
                    archive.by_index_decrypt(i, p.as_bytes())?
                } else {
                    archive.by_index(i)?
                };
                let out_file = File::create(&target_path)?;
                let mut buf_out = BufWriter::with_capacity(64 * 1024, out_file);
                copy(&mut file, &mut buf_out)?;
                buf_out.flush()?;
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
                            let (entry_idx, ref target_path) = files_ref[idx];
                            let res: Result<()> = (|| {
                                let mut file = if let Some(ref p) = pass {
                                    arch.by_index_decrypt(entry_idx, p.as_bytes())?
                                } else {
                                    arch.by_index(entry_idx)?
                                };
                                let out_file = File::create(target_path)?;
                                let mut buf_out = BufWriter::with_capacity(64 * 1024, out_file);
                                copy(&mut file, &mut buf_out)?;
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

impl ArchiveWriter for ZipHandler {
    fn create(archive_path: &Path, files: &[PathBuf], options: &CompressionOptions) -> Result<()> {
        let file = File::create(archive_path)
            .with_context(|| format!("Failed to create zip file: {}", archive_path.display()))?;
        let mut writer = ZipWriter::new(file);

        let (method, comp_level) = if options.level == 0 {
            (CompressionMethod::Stored, None)
        } else {
            (CompressionMethod::Deflated, Some(options.level as i64))
        };

        let file_options = SimpleFileOptions::default()
            .compression_method(method)
            .compression_level(comp_level);

        let dir_options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        let mut dir_entries: Vec<String> = Vec::new();
        let mut file_items: Vec<(PathBuf, String)> = Vec::new();

        for target in files {
            if target.is_dir() {
                let base = target.parent().unwrap_or(target);
                for entry in WalkDir::new(target) {
                    let entry = entry?;
                    let path = entry.path();
                    let rel_path = path.strip_prefix(base).unwrap_or(path);
                    let rel_name = rel_path.to_string_lossy().replace('\\', "/");

                    if path.is_dir() {
                        let dir_name = if rel_name.ends_with('/') { rel_name } else { format!("{}/", rel_name) };
                        dir_entries.push(dir_name);
                    } else if path.is_file() {
                        file_items.push((path.to_path_buf(), rel_name));
                    }
                }
            } else if target.is_file() {
                let file_name = target.file_name().unwrap_or_default().to_string_lossy().to_string();
                file_items.push((target.clone(), file_name));
            }
        }

        // Add all directory entries first
        for dir_name in dir_entries {
            writer.add_directory(dir_name, dir_options)?;
        }

        if file_items.is_empty() {
            writer.finish()?;
            return Ok(());
        }

        let num_threads = options.threads.unwrap_or_else(|| {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
        }).max(1);

        if num_threads <= 1 || file_items.len() <= 1 {
            // Sequential compression path
            for (path, rel_name) in file_items {
                writer.start_file(rel_name, file_options)?;
                let f = File::open(&path)?;
                let mut buf_f = BufReader::with_capacity(64 * 1024, f);
                copy(&mut buf_f, &mut writer)?;
            }
        } else {
            // Parallel bounded producer-consumer pipeline
            let batch_size = (file_items.len() / (num_threads * 4)).clamp(1, 32);
            let batches: Vec<Vec<(PathBuf, String)>> = file_items
                .chunks(batch_size)
                .map(|c| c.to_vec())
                .collect();
            let total_batches = batches.len();

            let bound = (num_threads * 2).max(4);
            let (tx, rx) = mpsc::sync_channel::<Result<(usize, Cursor<Vec<u8>>)>>(bound);

            let next_batch_idx = AtomicUsize::new(0);
            let worker_count = num_threads.min(total_batches);

            std::thread::scope(|s| {
                for _ in 0..worker_count {
                    let tx = tx.clone();
                    let next_batch_idx = &next_batch_idx;
                    let batches = &batches;
                    s.spawn(move || {
                        loop {
                            let idx = next_batch_idx.fetch_add(1, Ordering::Relaxed);
                            if idx >= batches.len() {
                                break;
                            }
                            let batch = &batches[idx];
                            let res: Result<Cursor<Vec<u8>>> = (|| {
                                let cursor = Cursor::new(Vec::new());
                                let mut sub_writer = ZipWriter::new(cursor);
                                for (path, rel_name) in batch {
                                    sub_writer.start_file(rel_name, file_options)?;
                                    let f = File::open(path)?;
                                    let mut buf_f = BufReader::with_capacity(64 * 1024, f);
                                    copy(&mut buf_f, &mut sub_writer)?;
                                }
                                let finished = sub_writer.finish()?;
                                Ok(finished)
                            })();

                            match res {
                                Ok(cursor) => {
                                    if tx.send(Ok((idx, cursor))).is_err() {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(Err(e));
                                    break;
                                }
                            }
                        }
                    });
                }

                drop(tx);

                let mut expected_idx = 0;
                let mut pending: BTreeMap<usize, Cursor<Vec<u8>>> = BTreeMap::new();

                while let Ok(msg) = rx.recv() {
                    let (idx, cursor) = msg?;
                    pending.insert(idx, cursor);
                    while let Some(cursor) = pending.remove(&expected_idx) {
                        let sub_archive = ZipArchive::new(cursor)?;
                        writer.merge_archive(sub_archive)?;
                        expected_idx += 1;
                    }
                }

                if expected_idx != total_batches {
                    anyhow::bail!("Parallel compression failed: processed {} of {} batches", expected_idx, total_batches);
                }

                Ok(())
            })?;
        }

        writer.finish()?;
        Ok(())
    }
}
