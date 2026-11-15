use anyhow::{Context, Result};
use bzip2::Compression as BzCompression;
use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use flate2::Compression as GzCompression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write, copy, sink};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::core::io as file_io;
use crate::core::{
    ArchiveEntry, ArchiveFormat, ArchiveReader, ArchiveWriter, CompressionOptions, ExtractOptions,
};

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
        let file = file_io::open_sequential(&self.path)
            .with_context(|| format!("Failed to open archive: {}", self.path.display()))?;
        let buf_file = BufReader::with_capacity(file_io::ARCHIVE_BUFFER_SIZE, file);
        match self.format {
            ArchiveFormat::Tar => Ok(Box::new(buf_file)),
            ArchiveFormat::TarGz => Ok(Box::new(GzDecoder::new(buf_file))),
            ArchiveFormat::TarBz2 => Ok(Box::new(BzDecoder::new(buf_file))),
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

        let mut known_dirs: HashSet<PathBuf> = HashSet::new();
        known_dirs.insert(options.output_dir.clone());

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
                if !options.flat && known_dirs.insert(target_path.clone()) {
                    fs::create_dir_all(&target_path)?;
                }
            } else {
                if let Some(parent) = target_path.parent() {
                    if known_dirs.insert(parent.to_path_buf()) {
                        fs::create_dir_all(parent)?;
                    }
                }
                let size = entry.size();
                file_io::copy_to_file(&mut entry, &target_path, size)?;
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
        let buf_file = BufWriter::with_capacity(file_io::ARCHIVE_BUFFER_SIZE, file);

        let format = ArchiveFormat::from_path(archive_path).unwrap_or(ArchiveFormat::Tar);

        let writer: Box<dyn Write> = match format {
            ArchiveFormat::Tar => Box::new(buf_file),
            ArchiveFormat::TarGz => {
                let level = GzCompression::new(options.level.clamp(0, 9) as u32);
                Box::new(GzEncoder::new(buf_file, level))
            }
            ArchiveFormat::TarBz2 => {
                let level = BzCompression::new(options.level.clamp(1, 9) as u32);
                Box::new(BzEncoder::new(buf_file, level))
            }
            _ => anyhow::bail!("Invalid tar archive target"),
        };

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
                        builder.append_dir(&rel_name, path)?;
                    } else if file_type.is_file() {
                        builder.append_path_with_name(path, &rel_name)?;
                    } else if file_type.is_symlink() {
                        if path.is_dir() {
                            builder.append_dir(&rel_name, path)?;
                        } else if path.is_file() {
                            builder.append_path_with_name(path, &rel_name)?;
                        }
                    }
                }
            } else if target_meta.is_file() {
                let file_name = target
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                builder.append_path_with_name(target, &file_name)?;
            }
        }

        let mut output = builder.into_inner()?;
        output.flush()?;
        Ok(())
    }
}
