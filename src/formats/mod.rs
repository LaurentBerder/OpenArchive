pub mod sevenz;
pub mod zip;
pub mod tar;
pub(crate) mod stream_compress;

use std::path::{Path, PathBuf};
use anyhow::Result;

use crate::core::{ArchiveFormat, ArchiveReader, ArchiveWriter, CompressionOptions};
use self::sevenz::SevenZHandler;
use self::zip::ZipHandler;
use self::tar::TarHandler;

pub fn get_reader<P: AsRef<Path>>(path: P, password: Option<String>) -> Result<Box<dyn ArchiveReader>> {
    let p = path.as_ref();
    let format = ArchiveFormat::from_path(p)
        .ok_or_else(|| anyhow::anyhow!("Unknown or unsupported archive format for: {}", p.display()))?;

    match format {
        ArchiveFormat::SevenZip => Ok(Box::new(SevenZHandler::new(p, password))),
        ArchiveFormat::Zip => Ok(Box::new(ZipHandler::new(p, password))),
        ArchiveFormat::Tar | ArchiveFormat::TarGz | ArchiveFormat::TarBz2 => {
            Ok(Box::new(TarHandler::new(p, format)))
        }
    }
}

pub fn create_archive(archive_path: &Path, files: &[PathBuf], options: &CompressionOptions) -> Result<()> {
    let format = ArchiveFormat::from_path(archive_path)
        .ok_or_else(|| anyhow::anyhow!("Cannot determine archive format from name: {}", archive_path.display()))?;

    match format {
        ArchiveFormat::SevenZip => SevenZHandler::create(archive_path, files, options),
        ArchiveFormat::Zip => ZipHandler::create(archive_path, files, options),
        ArchiveFormat::Tar | ArchiveFormat::TarGz | ArchiveFormat::TarBz2 => {
            TarHandler::create(archive_path, files, options)
        }
    }
}
