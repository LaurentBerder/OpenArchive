//! Safe mapping of untrusted archive entry names to filesystem paths.

use std::io;
use std::path::{Component, Path, PathBuf};

/// Converts an entry name stored in an archive into a path relative to the
/// extraction directory.
///
/// Archive entry names are untrusted input. Both `/` and `\` are treated as
/// separators, leading separators are dropped (as 7-Zip and GNU tar do) and
/// `.` segments are ignored. Names that could resolve outside the extraction
/// directory are rejected:
/// - `..` segments, and any segment made only of dots and spaces, because
///   Windows strips trailing dots and spaces and turns `.. ` into `..`;
/// - on Windows, segments containing `:`, which form drive prefixes (`C:`)
///   and alternate data streams (`file:stream`).
///
/// Returns an empty path for names that denote the extraction root itself,
/// such as `./`.
pub(crate) fn sanitize_entry_name(name: &str) -> io::Result<PathBuf> {
    let mut relative = PathBuf::new();

    for segment in name.split(['/', '\\']) {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment.chars().all(|c| c == '.' || c == ' ') {
            return Err(unsafe_entry(name, "contains a parent-directory component"));
        }
        if segment.contains('\0') {
            return Err(unsafe_entry(name, "contains a NUL character"));
        }
        if cfg!(windows) && segment.contains(':') {
            return Err(unsafe_entry(name, "contains a drive or stream prefix"));
        }
        relative.push(segment);
    }

    // Defense in depth: the checks above should already guarantee this.
    if !relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(unsafe_entry(name, "is not a plain relative path"));
    }

    Ok(relative)
}

/// Computes where an archive entry is written under `output_dir`.
///
/// With `flat`, only the entry's file name is kept. Returns `Ok(None)` when
/// the entry has no name to write (for example `./`), and an error when the
/// name is unsafe (see [`sanitize_entry_name`]).
pub(crate) fn entry_target(output_dir: &Path, name: &str, flat: bool) -> io::Result<Option<PathBuf>> {
    let relative = sanitize_entry_name(name)?;

    if flat {
        return Ok(relative.file_name().map(|file_name| output_dir.join(file_name)));
    }
    if relative.as_os_str().is_empty() {
        return Ok(None);
    }
    Ok(Some(output_dir.join(relative)))
}

/// Returns whether an archive entry is selected by the user's filters.
///
/// A filter selects the entry with exactly that path, and everything below it
/// when it names a directory: `docs` selects `docs`, `docs/` and `docs/a.txt`,
/// but not `docs2/a.txt` or `old/docs`. `/` and `\` are equivalent, and
/// leading `./` or `/` and trailing separators are ignored on both sides.
/// Matching is case-sensitive. No filters (or an empty list) selects
/// everything, as does a filter naming the root (`.` or `/`).
pub(crate) fn matches_filter(filter: Option<&[String]>, name: &str) -> bool {
    let Some(filters) = filter else {
        return true;
    };
    if filters.is_empty() {
        return true;
    }

    let name = trim_path(name).as_bytes();
    filters.iter().any(|filter| {
        let filter = trim_path(filter).as_bytes();
        if filter.is_empty() {
            return true;
        }
        name.len() >= filter.len()
            && name
                .iter()
                .zip(filter)
                .all(|(&a, &b)| a == b || (is_separator(a) && is_separator(b)))
            && (name.len() == filter.len() || is_separator(name[filter.len()]))
    })
}

fn is_separator(byte: u8) -> bool {
    byte == b'/' || byte == b'\\'
}

/// Strips leading `./` and separators and trailing separators.
fn trim_path(path: &str) -> &str {
    let mut path = path;
    loop {
        path = path.trim_start_matches(['/', '\\']);
        match path.strip_prefix('.') {
            Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => path = rest,
            _ => break,
        }
    }
    path.trim_end_matches(['/', '\\'])
}

fn unsafe_entry(name: &str, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("refusing to extract unsafe archive entry {name:?}: {reason}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(name: &str) -> PathBuf {
        sanitize_entry_name(name).unwrap()
    }

    #[test]
    fn keeps_ordinary_names() {
        assert_eq!(rel("a/b/c.txt"), Path::new("a").join("b").join("c.txt"));
        assert_eq!(rel("dir/"), PathBuf::from("dir"));
        assert_eq!(rel("a\\b.txt"), Path::new("a").join("b.txt"));
        assert_eq!(rel("..hidden"), PathBuf::from("..hidden"));
        assert_eq!(rel("file..txt"), PathBuf::from("file..txt"));
        assert_eq!(rel("a..b/c.."), Path::new("a..b").join("c.."));
        assert_eq!(rel("./dir\\file.txt"), Path::new("dir").join("file.txt"));
    }

    #[test]
    fn strips_root_and_current_dir_segments() {
        assert_eq!(rel("/etc/passwd"), Path::new("etc").join("passwd"));
        assert_eq!(rel("\\\\server\\share\\x"), Path::new("server").join("share").join("x"));
        assert_eq!(rel("./a/./b"), Path::new("a").join("b"));
        assert_eq!(rel("./"), PathBuf::new());
        assert_eq!(rel(""), PathBuf::new());
    }

    #[test]
    fn rejects_parent_traversal() {
        for name in [
            "..",
            "../x",
            "a/../../x",
            "a/..",
            "..\\x",
            "a\\..\\..\\x",
            "/../x",
            "... /x",
            ".. /x",
            "a/.../b",
        ] {
            assert!(sanitize_entry_name(name).is_err(), "{name:?} should be rejected");
        }
    }

    #[test]
    fn rejects_nul() {
        assert!(sanitize_entry_name("a\0b").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_drive_and_stream_prefixes_on_windows() {
        for name in ["C:\\Windows\\x", "C:x", "C:/x", "file.txt:stream", "\\\\?\\C:\\x"] {
            assert!(sanitize_entry_name(name).is_err(), "{name:?} should be rejected");
        }
    }

    fn selects(filters: &[&str], name: &str) -> bool {
        let filters: Vec<String> = filters.iter().map(|f| f.to_string()).collect();
        matches_filter(Some(&filters), name)
    }

    #[test]
    fn filter_matches_exact_paths_only() {
        assert!(selects(&["a.txt"], "a.txt"));
        assert!(!selects(&["a.txt"], "data.txt"));
        assert!(!selects(&["a.txt"], "a.txt.bak"));
        assert!(!selects(&["a.txt"], "dir/a.txt"));
        assert!(!selects(&["dir/a.txt"], "a.txt"));
        assert!(!selects(&["A.txt"], "a.txt"));
    }

    #[test]
    fn filter_selects_directory_contents() {
        assert!(selects(&["docs"], "docs"));
        assert!(selects(&["docs"], "docs/"));
        assert!(selects(&["docs"], "docs/a.txt"));
        assert!(selects(&["docs/"], "docs/sub/b.txt"));
        assert!(!selects(&["docs"], "docs2/a.txt"));
        assert!(!selects(&["docs"], "old/docs/a.txt"));
        assert!(!selects(&["docs/sub"], "docs/a.txt"));
    }

    #[test]
    fn filter_normalizes_separators_and_prefixes() {
        assert!(selects(&["docs\\a.txt"], "docs/a.txt"));
        assert!(selects(&["docs/a.txt"], "docs\\a.txt"));
        assert!(selects(&["./docs/a.txt"], "docs/a.txt"));
        assert!(selects(&["docs/a.txt"], "./docs/a.txt"));
        assert!(selects(&["/docs"], "docs/a.txt"));
        assert!(selects(&[".hidden"], ".hidden"));
        assert!(!selects(&[".hidden"], "hidden"));
    }

    #[test]
    fn filter_any_of_several_and_empty_cases() {
        assert!(selects(&["x", "docs"], "docs/a.txt"));
        assert!(!selects(&["x", "y"], "docs/a.txt"));
        assert!(matches_filter(None, "anything"));
        assert!(matches_filter(Some(&[]), "anything"));
        assert!(selects(&["."], "docs/a.txt"));
        assert!(selects(&["/"], "docs/a.txt"));
    }

    #[test]
    fn entry_target_stays_inside_output_dir() {
        let out = Path::new("out");
        assert_eq!(
            entry_target(out, "a/b.txt", false).unwrap(),
            Some(out.join("a").join("b.txt"))
        );
        assert_eq!(entry_target(out, "a/b.txt", true).unwrap(), Some(out.join("b.txt")));
        assert_eq!(entry_target(out, "./", false).unwrap(), None);
        assert_eq!(entry_target(out, "./", true).unwrap(), None);
        assert!(entry_target(out, "../x", false).is_err());
        assert!(entry_target(out, "../x", true).is_err());
    }
}
