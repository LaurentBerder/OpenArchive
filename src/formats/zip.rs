use std::fs::{self, File};
use std::io::{copy, sink};
use std::path::{Path, PathBuf};
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

        let pass = options.password.as_deref().or(self.password.as_deref());

        for i in 0..archive.len() {
            let mut file = if let Some(p) = pass {
                archive.by_index_decrypt(i, p.as_bytes())?
            } else {
                archive.by_index(i)?
            };

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
                    fs::create_dir_all(&target_path)?;
                }
            } else {
                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut out = File::create(&target_path)?;
                copy(&mut file, &mut out)?;
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

        let method = if options.level == 0 {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };

        let file_options = SimpleFileOptions::default().compression_method(method);

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
                        writer.add_directory(dir_name, file_options)?;
                    } else if path.is_file() {
                        writer.start_file(rel_name, file_options)?;
                        let mut f = File::open(path)?;
                        copy(&mut f, &mut writer)?;
                    }
                }
            } else if target.is_file() {
                let file_name = target.file_name().unwrap_or_default().to_string_lossy().to_string();
                writer.start_file(file_name, file_options)?;
                let mut f = File::open(target)?;
                copy(&mut f, &mut writer)?;
            }
        }

        writer.finish()?;
        Ok(())
    }
}
