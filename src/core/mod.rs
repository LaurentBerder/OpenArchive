use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    SevenZip,
    Zip,
    Tar,
    TarGz,
    TarBz2,
}

impl ArchiveFormat {
    pub fn from_path<P: AsRef<Path>>(path: P) -> Option<Self> {
        let path = path.as_ref();
        let file_name = path.file_name()?.to_str()?.to_lowercase();

        if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
            return Some(Self::TarGz);
        }
        if file_name.ends_with(".tar.bz2") || file_name.ends_with(".tbz2") {
            return Some(Self::TarBz2);
        }
        if file_name.ends_with(".7z") {
            return Some(Self::SevenZip);
        }
        if file_name.ends_with(".zip") {
            return Some(Self::Zip);
        }
        if file_name.ends_with(".tar") {
            return Some(Self::Tar);
        }

        // Try detecting by magic bytes if file exists
        if let Ok(mut file) = std::fs::File::open(path) {
            use std::io::Read;
            let mut magic = [0u8; 6];
            if file.read_exact(&mut magic).is_ok() {
                // 7z: 37 7A BC AF 27 1C
                if &magic == &[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C] {
                    return Some(Self::SevenZip);
                }
                // Zip: 50 4B 03 04 or 50 4B 05 06
                if &magic[0..2] == &[0x50, 0x4B] {
                    return Some(Self::Zip);
                }
                // Gz: 1F 8B
                if &magic[0..2] == &[0x1F, 0x8B] {
                    return Some(Self::TarGz);
                }
                // Bz2: 42 5A
                if &magic[0..2] == &[0x42, 0x5A] {
                    return Some(Self::TarBz2);
                }
            }
        }

        None
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::SevenZip => "7z",
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::TarBz2 => "tar.bz2",
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ArchiveEntry {
    pub name: String,
    pub size: u64,
    pub compressed_size: Option<u64>,
    pub is_dir: bool,
    pub modified: Option<DateTime<Utc>>,
    pub crc32: Option<u64>,
    pub encrypted: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CompressionOptions {
    pub level: u8, // 0 (Store) to 9 (Ultra)
    pub password: Option<String>,
    pub solid: bool,
}

impl Default for CompressionOptions {
    fn default() -> Self {
        Self {
            level: 5, // Normal
            password: None,
            solid: true,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ExtractOptions {
    pub output_dir: PathBuf,
    pub flat: bool,
    pub password: Option<String>,
    pub overwrite: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("."),
            flat: false,
            password: None,
            overwrite: true,
        }
    }
}

pub trait ArchiveReader {
    fn list(&mut self) -> anyhow::Result<Vec<ArchiveEntry>>;
    fn extract(&mut self, options: &ExtractOptions, filter: Option<&[String]>) -> anyhow::Result<()>;
    fn test(&mut self, password: Option<&str>) -> anyhow::Result<()>;
}

pub trait ArchiveWriter {
    fn create(archive_path: &Path, files: &[PathBuf], options: &CompressionOptions) -> anyhow::Result<()>;
}
