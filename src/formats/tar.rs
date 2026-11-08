use std::fs::{self, File};
use std::io::{copy, sink, Read, Write};
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use bzip2::Compression as BzCompression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression as GzCompression;
use walkdir::WalkDir;

use crate::core::{ArchiveEntry, ArchiveFormat, ArchiveReader, ArchiveWriter, CompressionOptions, ExtractOptions};

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

    fn open_reader(&self) -> Result<Box<dyn Read>> {
        let file = File::open(&self.path)
            .with_context(|| format!("Failed to open archive: {}", self.path.display()))?;
        match self.format {
            ArchiveFormat::Tar => Ok(Box::new(file)),
            ArchiveFormat::TarGz => Ok(Box::new(GzDecoder::new(file))),
            ArchiveFormat::TarBz2 => Ok(Box::new(BzDecoder::new(file))),
            _ => anyhow::bail!("Unsupported tar format variant"),
        }
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
        let reader = self.open_reader()?;
        let mut archive = tar::Archive::new(reader);

        fs::create_dir_all(&options.output_dir)?;

        for entry in archive.entries()? {
            let mut entry = entry?;
            let name = entry.path()?.to_string_lossy().to_string();

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

            if entry.header().entry_type().is_dir() {
                if !options.flat {
                    fs::create_dir_all(&target_path)?;
                }
            } else {
                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut out = File::create(&target_path)?;
                copy(&mut entry, &mut out)?;
            }
        }

        Ok(())
    }

    fn test(&mut self, _password: Option<&str>) -> Result<()> {
        let reader = self.open_reader()?;
        let mut archive = tar::Archive::new(reader);

        let mut out = sink();
        for entry in archive.entries()? {
            let mut entry = entry?;
            copy(&mut entry, &mut out)?;
        }

        Ok(())
    }
}

impl ArchiveWriter for TarHandler {
    fn create(archive_path: &Path, files: &[PathBuf], options: &CompressionOptions) -> Result<()> {
        let file = File::create(archive_path)
            .with_context(|| format!("Failed to create archive: {}", archive_path.display()))?;

        let format = ArchiveFormat::from_path(archive_path)
            .unwrap_or(ArchiveFormat::Tar);

        let writer: Box<dyn Write> = match format {
            ArchiveFormat::Tar => Box::new(file),
            ArchiveFormat::TarGz => {
                let level = GzCompression::new(options.level.clamp(0, 9) as u32);
                Box::new(GzEncoder::new(file, level))
            }
            ArchiveFormat::TarBz2 => {
                let level = BzCompression::new(options.level.clamp(1, 9) as u32);
                Box::new(BzEncoder::new(file, level))
            }
            _ => anyhow::bail!("Invalid tar archive target"),
        };

        let mut builder = tar::Builder::new(writer);

        for target in files {
            if target.is_dir() {
                let base = target.parent().unwrap_or(target);
                for entry in WalkDir::new(target) {
                    let entry = entry?;
                    let path = entry.path();
                    let rel_path = path.strip_prefix(base).unwrap_or(path);
                    let rel_name = rel_path.to_string_lossy().replace('\\', "/");

                    if path.is_dir() {
                        builder.append_dir(&rel_name, path)?;
                    } else if path.is_file() {
                        builder.append_path_with_name(path, &rel_name)?;
                    }
                }
            } else if target.is_file() {
                let file_name = target.file_name().unwrap_or_default().to_string_lossy().to_string();
                builder.append_path_with_name(target, &file_name)?;
            }
        }

        builder.finish()?;
        Ok(())
    }
}
