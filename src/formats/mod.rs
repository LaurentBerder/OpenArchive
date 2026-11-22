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
    let format = ArchiveFormat::detect(p)
        .ok_or_else(|| anyhow::anyhow!("Unknown or unsupported archive format for: {}", p.display()))?;

    match format {
        ArchiveFormat::SevenZip => Ok(Box::new(SevenZHandler::new(p, password))),
        ArchiveFormat::Zip => Ok(Box::new(ZipHandler::new(p, password))),
        ArchiveFormat::Tar | ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz => {
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
        ArchiveFormat::Tar | ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz => {
            TarHandler::create(archive_path, files, options)
        }
    }
}

/// Why an operation failed for lack of a usable password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordProblem {
    /// The archive is encrypted and no password was given.
    Required,
    /// A password was given but it does not decrypt the archive.
    Incorrect,
}

/// Classifies an archive error as a password problem, if it is one.
///
/// Library errors reach callers wrapped in `anyhow` context or inside
/// `io::Error`, so the whole chain is searched, including the payload of each
/// `io::Error` (whose `source()` skips it).
///
/// A cancellation is never a password problem, even though sevenz-rust2
/// reports any I/O error during an encrypted read as `MaybeBadPassword`.
pub fn password_problem(err: &anyhow::Error) -> Option<PasswordProblem> {
    if is_cancelled(err) {
        return None;
    }
    err.chain().find_map(classify_password_error)
}

/// Whether an operation failed because it was cancelled through its
/// [`crate::core::Progress`].
///
/// The [`crate::core::Cancelled`] marker travels inside an `io::Error`, which
/// sevenz-rust2 wraps in variants that expose no `source()`; those are opened
/// explicitly.
pub fn is_cancelled(err: &anyhow::Error) -> bool {
    err.chain().any(error_is_cancelled)
}

fn error_is_cancelled(err: &(dyn std::error::Error + 'static)) -> bool {
    if err.is::<crate::core::Cancelled>() {
        return true;
    }
    if let Some(err) = err.downcast_ref::<std::io::Error>() {
        return err.get_ref().is_some_and(|inner| error_is_cancelled(inner));
    }
    if let Some(err) = err.downcast_ref::<sevenz_rust2::Error>() {
        return match err {
            sevenz_rust2::Error::Io(inner, _)
            | sevenz_rust2::Error::FileOpen(inner, _)
            | sevenz_rust2::Error::MaybeBadPassword(inner) => error_is_cancelled(inner),
            _ => false,
        };
    }
    false
}

fn classify_password_error(err: &(dyn std::error::Error + 'static)) -> Option<PasswordProblem> {
    if let Some(err) = err.downcast_ref::<sevenz_rust2::Error>() {
        return match err {
            sevenz_rust2::Error::PasswordRequired => Some(PasswordProblem::Required),
            sevenz_rust2::Error::MaybeBadPassword(_) => Some(PasswordProblem::Incorrect),
            _ => None,
        };
    }
    if let Some(err) = err.downcast_ref::<::zip::result::ZipError>() {
        return match err {
            ::zip::result::ZipError::InvalidPassword => Some(PasswordProblem::Incorrect),
            ::zip::result::ZipError::UnsupportedArchive(msg)
                if *msg == ::zip::result::ZipError::PASSWORD_REQUIRED =>
            {
                Some(PasswordProblem::Required)
            }
            _ => None,
        };
    }
    if let Some(err) = err.downcast_ref::<std::io::Error>() {
        return err
            .get_ref()
            .and_then(|inner| classify_password_error(inner));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ExtractOptions;
    use ::zip::result::ZipError;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openarchive-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.txt"), vec![b'x'; 10_000]).unwrap();
        dir
    }

    #[test]
    fn classifies_encrypted_7z_errors() {
        let dir = temp_dir("pwclass");
        let archive = dir.join("enc.7z");
        let options = CompressionOptions { password: Some("right".into()), ..Default::default() };
        create_archive(&archive, &[dir.join("src")], &options).unwrap();

        let list = |pw: Option<&str>| get_reader(&archive, pw.map(String::from)).unwrap().list();
        let extract = |pw: Option<&str>| {
            let options = ExtractOptions {
                output_dir: dir.join("out"),
                password: pw.map(String::from),
                ..Default::default()
            };
            get_reader(&archive, pw.map(String::from)).unwrap().extract(&options, None)
        };

        let err = list(None).unwrap_err();
        assert_eq!(password_problem(&err), Some(PasswordProblem::Required), "{err:#}");
        let err = list(Some("wrong")).unwrap_err();
        assert_eq!(password_problem(&err), Some(PasswordProblem::Incorrect), "{err:#}");
        assert!(list(Some("right")).is_ok());

        let err = extract(None).unwrap_err();
        assert_eq!(password_problem(&err), Some(PasswordProblem::Required), "{err:#}");
        let err = extract(Some("wrong")).unwrap_err();
        assert_eq!(password_problem(&err), Some(PasswordProblem::Incorrect), "{err:#}");
        extract(Some("right")).unwrap();
        assert_eq!(std::fs::read(dir.join("out/src/a.txt")).unwrap(), vec![b'x'; 10_000]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classifies_encrypted_zip_errors() {
        let dir = temp_dir("pwclass-zip");
        // Two files so `threads: 4` takes the parallel extraction path.
        std::fs::write(dir.join("src/b.txt"), vec![b'y'; 10_000]).unwrap();
        let archive = dir.join("enc.zip");
        let options = CompressionOptions { password: Some("right".into()), ..Default::default() };
        create_archive(&archive, &[dir.join("src")], &options).unwrap();

        // Zip headers are not encrypted: listing works and flags the entries.
        let entries = get_reader(&archive, None).unwrap().list().unwrap();
        assert!(entries.iter().filter(|e| !e.is_dir).all(|e| e.encrypted));

        for threads in [1, 4] {
            let extract = |pw: Option<&str>| {
                let options = ExtractOptions {
                    output_dir: dir.join(format!("out{threads}")),
                    password: pw.map(String::from),
                    threads: Some(threads),
                    ..Default::default()
                };
                get_reader(&archive, pw.map(String::from)).unwrap().extract(&options, None)
            };

            let err = extract(None).unwrap_err();
            assert_eq!(password_problem(&err), Some(PasswordProblem::Required), "{threads}: {err:#}");
            let err = extract(Some("wrong")).unwrap_err();
            assert_eq!(password_problem(&err), Some(PasswordProblem::Incorrect), "{threads}: {err:#}");
            extract(Some("right")).unwrap();
            assert_eq!(
                std::fs::read(dir.join(format!("out{threads}/src/b.txt"))).unwrap(),
                vec![b'y'; 10_000]
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classifies_zip_errors_through_wrappers() {
        let required = || ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED);

        let err = anyhow::Error::new(ZipError::InvalidPassword).context("opening");
        assert_eq!(password_problem(&err), Some(PasswordProblem::Incorrect));

        let err = anyhow::Error::new(required());
        assert_eq!(password_problem(&err), Some(PasswordProblem::Required));

        // ZipError converted to io::Error, then wrapped with context.
        let io: std::io::Error = std::io::Error::other(required());
        let err = anyhow::Error::new(io).context("worker failed");
        assert_eq!(password_problem(&err), Some(PasswordProblem::Required));
    }

    #[test]
    fn other_errors_are_not_password_problems() {
        let err = anyhow::Error::new(ZipError::UnsupportedArchive("multi-disk"));
        assert_eq!(password_problem(&err), None);
        let err = anyhow::Error::new(sevenz_rust2::Error::ChecksumVerificationFailed);
        assert_eq!(password_problem(&err), None);
        let err = anyhow::anyhow!("disk full");
        assert_eq!(password_problem(&err), None);
    }

    // ── Progress and cancellation (OA-26) ────────────────────────────────────

    use crate::core::Progress;
    use std::sync::Arc;

    /// `src/a.txt` (10 000 bytes) plus `src/big.bin` (3 MB) in a `.{ext}`.
    fn progress_archive(tag: &str, ext: &str, password: Option<&str>) -> (PathBuf, PathBuf) {
        let dir = temp_dir(tag);
        let big: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join("src/big.bin"), big).unwrap();
        let archive = dir.join(format!("p.{ext}"));
        let options = CompressionOptions { password: password.map(String::from), ..Default::default() };
        create_archive(&archive, &[dir.join("src")], &options).unwrap();
        (dir, archive)
    }

    const PAYLOAD_BYTES: u64 = 10_000 + 3 * 1024 * 1024;

    fn extract_with(archive: &Path, out: &Path, password: Option<&str>, threads: usize, progress: &Arc<Progress>) -> Result<()> {
        let options = ExtractOptions {
            output_dir: out.to_path_buf(),
            password: password.map(String::from),
            threads: Some(threads),
            progress: Some(progress.clone()),
            ..Default::default()
        };
        get_reader(archive, password.map(String::from))?.extract(&options, None)
    }

    fn files_in(dir: &Path) -> usize {
        walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).count()
    }

    #[test]
    fn extraction_reports_progress() {
        for ext in ["zip", "7z", "tar", "tar.gz"] {
            for threads in [1, 4] {
                let (dir, archive) = progress_archive(&format!("prog-{}", ext.replace('.', "")), ext, None);
                let progress = Arc::new(Progress::new());
                extract_with(&archive, &dir.join("out"), None, threads, &progress).unwrap();
                assert_eq!(progress.done(), PAYLOAD_BYTES, "{ext} t{threads}: bytes");
                match progress.total() {
                    // Streamed TAR cannot know the total up front.
                    None => assert!(ext == "tar.gz" || (ext == "tar" && threads == 1), "{ext} t{threads}: total"),
                    Some(total) => assert_eq!(total, PAYLOAD_BYTES, "{ext} t{threads}: total"),
                }
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }

    #[test]
    fn test_reports_progress() {
        for ext in ["zip", "7z", "tar.gz"] {
            let (dir, archive) = progress_archive(&format!("progt-{}", ext.replace('.', "")), ext, None);
            let progress = Progress::new();
            get_reader(&archive, None).unwrap().test(None, Some(&progress)).unwrap();
            assert_eq!(progress.done(), PAYLOAD_BYTES, "{ext}");
            if ext != "tar.gz" {
                assert_eq!(progress.total(), Some(PAYLOAD_BYTES), "{ext}");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn cancelled_extraction_stops_without_leaving_files() {
        for ext in ["zip", "7z", "tar", "tar.gz"] {
            for threads in [1, 4] {
                let (dir, archive) = progress_archive(&format!("cancel-{}", ext.replace('.', "")), ext, None);
                let progress = Arc::new(Progress::new());
                progress.cancel();
                let out = dir.join("out");
                let err = extract_with(&archive, &out, None, threads, &progress).unwrap_err();
                assert!(is_cancelled(&err), "{ext} t{threads}: {err:#}");
                assert_eq!(files_in(&out), 0, "{ext} t{threads}: files written");
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }

    #[test]
    fn cancelled_test_is_reported_as_cancelled() {
        for ext in ["zip", "7z", "tar.gz"] {
            let (dir, archive) = progress_archive(&format!("cancelt-{}", ext.replace('.', "")), ext, None);
            let progress = Progress::new();
            progress.cancel();
            let err = get_reader(&archive, None).unwrap().test(None, Some(&progress)).unwrap_err();
            assert!(is_cancelled(&err), "{ext}: {err:#}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn cancelling_an_encrypted_7z_is_not_a_password_problem() {
        // sevenz-rust2 rewraps I/O errors during an encrypted read as
        // MaybeBadPassword; a cancel must still read as a cancel.
        let (dir, archive) = progress_archive("cancel-enc", "7z", Some("right"));
        for threads in [1, 4] {
            let progress = Arc::new(Progress::new());
            progress.cancel();
            let err = extract_with(&archive, &dir.join("out"), Some("right"), threads, &progress).unwrap_err();
            assert!(is_cancelled(&err), "t{threads}: {err:#}");
            assert_eq!(password_problem(&err), None, "t{threads}: {err:#}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── TAR links and special entries (OA-7) ─────────────────────────────────

    /// A TAR with raw names and link targets: `::tar::Builder` refuses `..` in
    /// names it sets itself, so header fields are written directly.
    fn tar_entries(entries: &[(&str, ::tar::EntryType, &str, &[u8])]) -> Vec<u8> {
        let mut builder = ::tar::Builder::new(Vec::new());
        for (name, entry_type, link, data) in entries {
            let mut header = ::tar::Header::new_gnu();
            header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
            header.set_entry_type(*entry_type);
            header.set_mode(0o644);
            header.set_size(data.len() as u64);
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn write_tar_variant(dir: &Path, stem: &str, bytes: &[u8], gz: bool) -> PathBuf {
        use std::io::Write;
        if gz {
            let path = dir.join(format!("{stem}.tar.gz"));
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(bytes).unwrap();
            std::fs::write(&path, encoder.finish().unwrap()).unwrap();
            path
        } else {
            let path = dir.join(format!("{stem}.tar"));
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    #[test]
    fn tar_hard_links_are_copied_and_other_links_skipped() {
        use ::tar::EntryType::*;
        let bytes = tar_entries(&[
            ("pax_global_header", XGlobalHeader, "", b"52 comment=abc
"),
            ("a.txt", Regular, "", b"hello"),
            ("h.txt", Link, "a.txt", b""),
            ("s.txt", Symlink, "a.txt", b""),
            ("evil", Symlink, "../../x", b""),
            ("h_unsafe", Link, "../outside", b""),
            ("h_foreign", Link, "never.txt", b""),
            ("fifo", Fifo, "", b""),
        ]);

        for (gz, threads) in [(false, 1), (false, 4), (true, 1)] {
            let dir = temp_dir(&format!("tarlinks-{gz}-{threads}"));
            let archive = write_tar_variant(&dir, "links", &bytes, gz);
            let out = dir.join("out");
            std::fs::create_dir_all(&out).unwrap();
            // Exists in the output folder but is not from this archive.
            std::fs::write(out.join("never.txt"), b"keep").unwrap();

            let progress = Arc::new(Progress::new());
            extract_with(&archive, &out, None, threads, &progress).unwrap();
            let label = format!("gz={gz} t{threads}");

            assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"hello", "{label}");
            // The hard link is a real file with the same content, not a link.
            let h = std::fs::symlink_metadata(out.join("h.txt")).unwrap();
            assert!(h.is_file() && !h.file_type().is_symlink(), "{label}");
            assert_eq!(std::fs::read(out.join("h.txt")).unwrap(), b"hello", "{label}");

            for skipped in ["s.txt", "evil", "h_unsafe", "h_foreign", "fifo", "pax_global_header"] {
                assert!(std::fs::symlink_metadata(out.join(skipped)).is_err(), "{label}: {skipped} was created");
            }
            assert!(!dir.join("x").exists() && !dir.join("outside").exists(), "{label}");
            assert_eq!(std::fs::read(out.join("never.txt")).unwrap(), b"keep", "{label}");

            let warnings = progress.warnings();
            assert_eq!(warnings.len(), 5, "{label}: {warnings:?}");
            assert!(warnings.iter().any(|w| w == "skipped symbolic link s.txt -> a.txt"), "{label}: {warnings:?}");
            assert!(warnings.iter().any(|w| w.starts_with("skipped hard link h_foreign -> never.txt")), "{label}");
            assert!(warnings.iter().any(|w| w.starts_with("skipped special file fifo")), "{label}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn git_style_tar_keeps_the_parallel_path() {
        use ::tar::EntryType::*;
        let big = vec![b'z'; 200_000];
        let bytes = tar_entries(&[
            ("pax_global_header", XGlobalHeader, "", b"52 comment=abc
"),
            ("a.txt", Regular, "", b"one"),
            ("b.txt", Regular, "", &big),
        ]);
        let dir = temp_dir("gitstyle");
        let archive = write_tar_variant(&dir, "git", &bytes, false);
        let progress = Arc::new(Progress::new());
        extract_with(&archive, &dir.join("out"), None, 4, &progress).unwrap();

        assert!(!dir.join("out/pax_global_header").exists());
        assert_eq!(std::fs::read(dir.join("out/a.txt")).unwrap(), b"one");
        // Only the indexed (parallel) path knows the total up front.
        assert_eq!(progress.total(), Some(3 + 200_000));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
