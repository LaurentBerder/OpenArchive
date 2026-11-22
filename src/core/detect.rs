//! Recognising an archive's format from its content.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use super::ArchiveFormat;

const SEVEN_Z_MAGIC: &[u8] = &[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];
const XZ_MAGIC: &[u8] = &[0xFD, b'7', b'z', b'X', b'Z', 0x00];
const GZIP_MAGIC: &[u8] = &[0x1F, 0x8B];
/// Local file header, empty archive (end of central directory), spanned.
const ZIP_MAGICS: [&[u8]; 3] = [b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"];

const TAR_BLOCK: usize = 512;

/// Detects the format from the first bytes of the file, or `None` if it is
/// not a format OpenArchive reads. Compressed streams (gzip, bzip2, xz) are
/// only reported as `tar.*` when their first decompressed block is a TAR
/// header: a plain `.gz` of a text file is not an archive.
pub(super) fn detect_content(path: &Path) -> Option<ArchiveFormat> {
    let mut file = File::open(path).ok()?;
    let mut head = Vec::with_capacity(TAR_BLOCK);
    file.by_ref().take(TAR_BLOCK as u64).read_to_end(&mut head).ok()?;

    if head.starts_with(SEVEN_Z_MAGIC) {
        return Some(ArchiveFormat::SevenZip);
    }
    if ZIP_MAGICS.iter().any(|magic| head.starts_with(magic)) {
        return Some(ArchiveFormat::Zip);
    }

    // Compressed TAR: identified by the stream magic, confirmed by peeking.
    let compressed = if head.starts_with(GZIP_MAGIC) {
        Some(ArchiveFormat::TarGz)
    } else if is_bzip2(&head) {
        Some(ArchiveFormat::TarBz2)
    } else if head.starts_with(XZ_MAGIC) {
        Some(ArchiveFormat::TarXz)
    } else {
        None
    };
    if let Some(format) = compressed {
        let block = first_decompressed_block(path, format).ok()?;
        return is_tar_header(&block).then_some(format);
    }

    is_tar_header(&head).then_some(ArchiveFormat::Tar)
}

/// `BZh` followed by the block-size digit `1`–`9`; a bare `BZ` is not enough.
fn is_bzip2(head: &[u8]) -> bool {
    head.len() >= 4 && head.starts_with(b"BZh") && (b'1'..=b'9').contains(&head[3])
}

fn first_decompressed_block(path: &Path, format: ArchiveFormat) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let reader: Box<dyn Read> = match format {
        ArchiveFormat::TarGz => Box::new(flate2::read::MultiGzDecoder::new(file)),
        ArchiveFormat::TarBz2 => Box::new(bzip2::read::MultiBzDecoder::new(file)),
        ArchiveFormat::TarXz => Box::new(lzma_rust2::XzReader::new(file, true)),
        _ => return Err(io::Error::other("not a compressed stream")),
    };
    let mut block = Vec::with_capacity(TAR_BLOCK);
    reader.take(TAR_BLOCK as u64).read_to_end(&mut block)?;
    Ok(block)
}

/// Whether `block` is a TAR header: its stored checksum must match. That
/// covers old v7 archives without the `ustar` marker as well as POSIX and
/// GNU ones. An all-zero block (an empty archive's end marker) counts too.
fn is_tar_header(block: &[u8]) -> bool {
    if block.len() < TAR_BLOCK {
        return false;
    }
    let header = &block[..TAR_BLOCK];
    if header.iter().all(|&b| b == 0) {
        return true;
    }

    // Checksum field: bytes 148..156, octal, terminated by NUL or space.
    let field = &header[148..156];
    let digits: Vec<u8> = field
        .iter()
        .copied()
        .skip_while(|&b| b == b' ' || b == 0)
        .take_while(|&b| (b'0'..=b'7').contains(&b))
        .collect();
    if digits.is_empty() {
        return false;
    }
    let Ok(stored) = u32::from_str_radix(std::str::from_utf8(&digits).unwrap_or(""), 8) else {
        return false;
    };

    // The checksum is computed with the field itself read as spaces.
    let unsigned: u32 = header
        .iter()
        .enumerate()
        .map(|(i, &b)| if (148..156).contains(&i) { u32::from(b' ') } else { u32::from(b) })
        .sum();
    // Some old writers summed signed bytes.
    let signed: i64 = header
        .iter()
        .enumerate()
        .map(|(i, &b)| if (148..156).contains(&i) { i64::from(b' ') } else { i64::from(b as i8) })
        .sum();
    stored == unsigned || i64::from(stored) == signed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CompressionOptions;
    use crate::formats::create_archive;
    use std::io::Write;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openarchive-detect-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.txt"), b"hello").unwrap();
        dir
    }

    #[test]
    fn detects_every_format_without_an_extension() {
        let dir = scratch("noext");
        for (ext, format) in [
            ("7z", ArchiveFormat::SevenZip),
            ("zip", ArchiveFormat::Zip),
            ("tar", ArchiveFormat::Tar),
            ("tar.gz", ArchiveFormat::TarGz),
            ("tar.bz2", ArchiveFormat::TarBz2),
            ("tar.xz", ArchiveFormat::TarXz),
        ] {
            let named = dir.join(format!("a.{ext}"));
            create_archive(&named, &[dir.join("src")], &CompressionOptions::default()).unwrap();
            let bare = dir.join(format!("bare-{}", ext.replace('.', "-")));
            std::fs::rename(&named, &bare).unwrap();
            assert_eq!(ArchiveFormat::detect(&bare), Some(format), "{ext}");
            assert_eq!(ArchiveFormat::from_path(&bare), Some(format), "{ext}: from_path fallback");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_wins_over_a_wrong_extension_when_opening() {
        let dir = scratch("misnamed");
        let real = dir.join("real.7z");
        create_archive(&real, &[dir.join("src")], &CompressionOptions::default()).unwrap();
        let misnamed = dir.join("actually-7z.zip");
        std::fs::rename(&real, &misnamed).unwrap();

        assert_eq!(ArchiveFormat::detect(&misnamed), Some(ArchiveFormat::SevenZip));
        // Creating still follows the name.
        assert_eq!(ArchiveFormat::from_path(&misnamed), Some(ArchiveFormat::Zip));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn look_alikes_are_not_archives() {
        let dir = scratch("lookalike");
        let cases: [(&str, Vec<u8>); 4] = [
            ("bz-text", b"BZ is not a bzip2 header".to_vec()),
            ("pk-text", b"PK but no zip signature".to_vec()),
            ("plain", b"just some text".to_vec()),
            ("gz-text", {
                // A real gzip stream whose content is not a TAR.
                let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                gz.write_all(&b"hello world, not a tar ".repeat(100)).unwrap();
                gz.finish().unwrap()
            }),
        ];
        for (name, bytes) in cases {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(ArchiveFormat::detect(&path), None, "{name}");
            assert_eq!(ArchiveFormat::from_path(&path), None, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_v7_tar_without_ustar_magic_is_detected() {
        let dir = scratch("v7");
        let mut builder = ::tar::Builder::new(Vec::new());
        let mut header = ::tar::Header::new_old(); // v7: no "ustar" magic
        header.set_size(5);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, "a.txt", &b"hello"[..]).unwrap();
        let bytes = builder.into_inner().unwrap();
        assert_ne!(&bytes[257..262], b"ustar");

        let path = dir.join("old-archive");
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(ArchiveFormat::detect(&path), Some(ArchiveFormat::Tar));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tar_checksum_rejects_corrupted_headers() {
        let mut header = ::tar::Header::new_gnu();
        header.set_path("a.txt").unwrap();
        header.set_size(1);
        header.set_cksum();
        let mut block = header.as_bytes().to_vec();
        assert!(is_tar_header(&block));
        block[0] ^= 0x01;
        assert!(!is_tar_header(&block));
    }
}
