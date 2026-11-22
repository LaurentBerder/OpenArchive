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
pub fn password_problem(err: &anyhow::Error) -> Option<PasswordProblem> {
    err.chain().find_map(classify_password_error)
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
}
