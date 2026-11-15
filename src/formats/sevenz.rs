use std::fs::{self, File};
use std::io::{self, copy, Read};
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use sevenz_rust::{
    lzma::LZMA2Options, AesEncoderOptions, MethodOptions, Password, SeqReader, SevenZArchiveEntry,
    SevenZMethod, SevenZMethodConfiguration, SevenZReader, SevenZWriter, SourceReader,
};
use walkdir::WalkDir;

use crate::core::{ArchiveEntry, ArchiveReader, ArchiveWriter, CompressionOptions, ExtractOptions};

struct LazyFileReader {
    path: PathBuf,
    file: Option<File>,
    end: bool,
}

impl LazyFileReader {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            end: false,
        }
    }
}

impl Read for LazyFileReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.end {
            return Ok(0);
        }
        if self.file.is_none() {
            self.file = Some(File::open(&self.path)?);
        }
        let n = self.file.as_mut().unwrap().read(buf)?;
        if n == 0 {
            self.end = true;
            self.file = None; // Release OS file handle immediately
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
        let pass = self.password.as_deref().map(Password::from).unwrap_or_else(Password::empty);
        let reader = SevenZReader::open(&self.path, pass)
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
        let pass = options.password.as_deref()
            .or(self.password.as_deref())
            .map(Password::from)
            .unwrap_or_else(Password::empty);

        let mut reader = SevenZReader::open(&self.path, pass)
            .with_context(|| format!("Failed to open 7z archive: {}", self.path.display()))?;

        fs::create_dir_all(&options.output_dir)?;

        reader.for_each_entries(|entry, stream| {
            let name = entry.name();

            // Filter check
            if let Some(filters) = filter {
                if !filters.is_empty() && !filters.iter().any(|f| name.contains(f)) {
                    return Ok(true);
                }
            }

            let target_path = if options.flat {
                let file_name = Path::new(name).file_name().unwrap_or_default();
                options.output_dir.join(file_name)
            } else {
                options.output_dir.join(name)
            };

            if entry.is_directory() {
                if !options.flat {
                    fs::create_dir_all(&target_path)?;
                }
            } else {
                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                let mut out_file = File::create(&target_path)?;
                copy(stream, &mut out_file)?;
            }

            Ok(true)
        })?;

        Ok(())
    }

    fn test(&mut self, password: Option<&str>) -> Result<()> {
        let pass = password
            .or(self.password.as_deref())
            .map(Password::from)
            .unwrap_or_else(Password::empty);

        let mut reader = SevenZReader::open(&self.path, pass)
            .with_context(|| format!("Failed to open 7z archive: {}", self.path.display()))?;

        let mut sink = io::sink();
        reader.for_each_entries(|_entry, stream| {
            copy(stream, &mut sink)?;
            Ok(true)
        })?;

        Ok(())
    }
}

impl ArchiveWriter for SevenZHandler {
    fn create(archive_path: &Path, files: &[PathBuf], options: &CompressionOptions) -> Result<()> {
        let mut lzma_opts = LZMA2Options::with_preset(options.level.clamp(0, 9) as u32);
        if let Some(dict_size) = options.dict_size {
            lzma_opts.dict_size = dict_size;
        }

        let mut content_methods: Vec<SevenZMethodConfiguration> = Vec::new();
        if let Some(ref pwd) = options.password {
            if !pwd.is_empty() {
                content_methods.push(
                    AesEncoderOptions::new(pwd.as_str().into()).into(),
                );
            }
        }
        content_methods.push(
            SevenZMethodConfiguration::new(SevenZMethod::LZMA2)
                .with_options(MethodOptions::LZMA2(lzma_opts)),
        );

        let mut writer = SevenZWriter::create(archive_path)
            .with_context(|| format!("Failed to create 7z archive: {}", archive_path.display()))?;
        writer.set_content_methods(content_methods);

        // Separate directories and file items to support solid block compression
        let mut dir_entries: Vec<SevenZArchiveEntry> = Vec::new();
        let mut file_items: Vec<(SevenZArchiveEntry, PathBuf, u64)> = Vec::new();

        for target in files {
            if target.is_dir() {
                let base = target.parent().unwrap_or(target);
                for entry in WalkDir::new(target) {
                    let entry = entry?;
                    let path = entry.path();
                    let rel_path = path.strip_prefix(base).unwrap_or(path);
                    let rel_name = rel_path.to_string_lossy().replace('\\', "/");

                    if path.is_dir() {
                        let archive_entry = SevenZArchiveEntry::from_path(path, rel_name);
                        dir_entries.push(archive_entry);
                    } else if path.is_file() {
                        let size = entry.metadata()?.len();
                        let archive_entry = SevenZArchiveEntry::from_path(path, rel_name);
                        file_items.push((archive_entry, path.to_path_buf(), size));
                    }
                }
            } else if target.is_file() {
                let file_name = target.file_name().unwrap_or_default().to_string_lossy().to_string();
                let size = target.metadata()?.len();
                let archive_entry = SevenZArchiveEntry::from_path(target, file_name);
                file_items.push((archive_entry, target.clone(), size));
            }
        }

        // 1. Add all directory entries first
        for dir_entry in dir_entries {
            writer.push_archive_entry::<&[u8]>(dir_entry, None)?;
        }

        // 2. Add files: solid blocks vs non-solid blocks
        if options.solid {
            const MAX_BLOCK_SIZE: u64 = 4 * 1024 * 1024 * 1024; // 4 GB per solid block
            let mut batch_entries = Vec::new();
            let mut batch_readers = Vec::new();
            let mut batch_size: u64 = 0;

            for (entry, path, size) in file_items {
                if size >= MAX_BLOCK_SIZE {
                    if !batch_entries.is_empty() {
                        writer.push_archive_entries(
                            std::mem::take(&mut batch_entries),
                            SeqReader::new(std::mem::take(&mut batch_readers)),
                        )?;
                        batch_size = 0;
                    }
                    writer.push_archive_entries(
                        vec![entry],
                        SeqReader::new(vec![SourceReader::from(LazyFileReader::new(path))]),
                    )?;
                    continue;
                }

                if batch_size + size >= MAX_BLOCK_SIZE && !batch_entries.is_empty() {
                    writer.push_archive_entries(
                        std::mem::take(&mut batch_entries),
                        SeqReader::new(std::mem::take(&mut batch_readers)),
                    )?;
                    batch_size = 0;
                }

                batch_size += size;
                batch_entries.push(entry);
                batch_readers.push(SourceReader::from(LazyFileReader::new(path)));
            }

            if !batch_entries.is_empty() {
                writer.push_archive_entries(
                    batch_entries,
                    SeqReader::new(batch_readers),
                )?;
            }
        } else {
            // Non-solid mode: each file is compressed in an individual block
            for (entry, path, _size) in file_items {
                let file = File::open(&path)?;
                writer.push_archive_entry(entry, Some(file))?;
            }
        }

        writer.finish()?;
        Ok(())
    }
}
