use std::fs::{self, File};
use std::io::{self, copy};
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use sevenz_rust::{Password, SevenZArchiveEntry, SevenZReader, SevenZWriter};
use walkdir::WalkDir;

use crate::core::{ArchiveEntry, ArchiveReader, ArchiveWriter, CompressionOptions, ExtractOptions};

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
    fn create(archive_path: &Path, files: &[PathBuf], _options: &CompressionOptions) -> Result<()> {
        let mut writer = SevenZWriter::create(archive_path)
            .with_context(|| format!("Failed to create 7z archive: {}", archive_path.display()))?;

        for target in files {
            if target.is_dir() {
                let base = target.parent().unwrap_or(target);
                for entry in WalkDir::new(target) {
                    let entry = entry?;
                    let path = entry.path();
                    let rel_path = path.strip_prefix(base).unwrap_or(path);
                    let rel_name = rel_path.to_string_lossy().replace('\\', "/");

                    let archive_entry = SevenZArchiveEntry::from_path(path, rel_name);
                    if path.is_file() {
                        let file = File::open(path)?;
                        writer.push_archive_entry(archive_entry, Some(file))?;
                    } else if path.is_dir() {
                        writer.push_archive_entry(archive_entry, None::<File>)?;
                    }
                }
            } else if target.is_file() {
                let file_name = target.file_name().unwrap_or_default().to_string_lossy().to_string();
                let archive_entry = SevenZArchiveEntry::from_path(target, file_name);
                let file = File::open(target)?;
                writer.push_archive_entry(archive_entry, Some(file))?;
            }
        }

        writer.finish()?;
        Ok(())
    }
}
