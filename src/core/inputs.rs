//! Files and folders to add to an archive, and the names they get in it.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use super::filter::{EntryFilter, has_wildcard};

/// A file or folder to add, and its name in the archive. A folder is added
/// with everything below it, under that name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub path: PathBuf,
    pub name: String,
}

impl Input {
    /// Adds `path` under its own file name: `docs/a.txt` becomes `a.txt`, and
    /// the folder `src/docs` becomes `docs/…`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        Self { path, name }
    }

    /// Adds `path` as `name` (`/`-separated).
    pub fn named(path: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        Self { path: path.into(), name: name.into() }
    }
}

/// One file or directory entry to write.
pub(crate) struct Source {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub metadata: fs::Metadata,
}

/// Lists the entries to write for `inputs`, folders first walked in full.
///
/// Inputs that do not exist are skipped (callers report them). Symbolic links
/// are stored as what they point to; links to folders are not followed into,
/// and broken links are skipped. An entry reached twice (for example a file
/// also inside a listed folder) is written once, under its first name.
pub(crate) fn sources(inputs: &[Input]) -> io::Result<Vec<Source>> {
    let mut sources = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |source: Source| {
        if !source.name.is_empty() && seen.insert(source.name.clone()) {
            sources.push(source);
        }
    };

    for input in inputs {
        let Ok(metadata) = input.path.metadata() else {
            continue;
        };
        if !metadata.is_dir() {
            if metadata.is_file() {
                push(Source { path: input.path.clone(), name: input.name.clone(), is_dir: false, metadata });
            }
            continue;
        }
        for entry in WalkDir::new(&input.path) {
            let entry = entry?;
            let path = entry.path();
            let name = child_name(&input.name, path.strip_prefix(&input.path).unwrap_or(path));
            let metadata = if entry.file_type().is_symlink() {
                match path.metadata() {
                    Ok(metadata) => metadata,
                    Err(_) => continue, // broken link
                }
            } else {
                entry.metadata()?
            };
            if metadata.is_dir() || metadata.is_file() {
                push(Source { path: path.to_path_buf(), name, is_dir: metadata.is_dir(), metadata });
            }
        }
    }
    Ok(sources)
}

/// Command-line names expanded into inputs.
#[derive(Debug, Default)]
pub struct Expanded {
    pub inputs: Vec<Input>,
    /// Names that matched nothing (missing files, wildcards without a match).
    pub unmatched: Vec<String>,
}

/// Expands the names given to `a`, like 7-Zip.
///
/// A name without wildcards is added as is (a folder with everything below
/// it). A name whose last part has `*` or `?` adds the matching files and
/// folders of its folder, stored without that folder's path: `src/*.rs`
/// stores `main.rs`. With `recurse` (`-r`), matches in subfolders are added
/// too, stored with their path below that folder: `sub/lib.rs`. Wildcards
/// are case-insensitive on Windows. Wildcards before the last part
/// (`*/a.txt`) are an error.
pub fn expand(names: &[PathBuf], recurse: bool) -> Result<Expanded, String> {
    let mut expanded = Expanded::default();
    for name in names {
        let text = name.to_string_lossy();
        let (parent, pattern) = match text.rfind(['/', '\\']) {
            Some(i) => (&text[..i + 1], &text[i + 1..]),
            None => ("", &text[..]),
        };
        if has_wildcard(parent) {
            return Err(format!("wildcards are only supported in the last part of a name: {text}"));
        }
        if !has_wildcard(pattern) {
            if name.exists() {
                expanded.inputs.push(Input::new(name));
            } else {
                expanded.unmatched.push(text.into_owned());
            }
            continue;
        }

        let base = if parent.is_empty() { Path::new(".") } else { Path::new(parent) };
        let filter = EntryFilter::patterns([pattern], false);
        let mut matches: Vec<Input> = WalkDir::new(base)
            .min_depth(1)
            .max_depth(if recurse { usize::MAX } else { 1 })
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| filter.matches(&entry.file_name().to_string_lossy()))
            .map(|entry| {
                let relative = entry.path().strip_prefix(base).unwrap_or(entry.path());
                Input::named(entry.path(), child_name("", relative))
            })
            .collect();
        if matches.is_empty() {
            expanded.unmatched.push(text.into_owned());
        }
        expanded.inputs.append(&mut matches);
    }
    Ok(expanded)
}

/// `base/relative` with `/` separators; `base` alone for the folder itself.
fn child_name(base: &str, relative: &Path) -> String {
    let relative = relative.to_string_lossy().replace('\\', "/");
    match (base.is_empty(), relative.is_empty()) {
        (_, true) => base.to_string(),
        (true, false) => relative,
        (false, false) => format!("{base}/{relative}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oa_inputs_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(inputs: &[Input]) -> Vec<(String, bool)> {
        let mut names: Vec<_> = sources(inputs).unwrap().into_iter().map(|s| (s.name, s.is_dir)).collect();
        names.sort();
        names
    }

    #[test]
    fn files_folders_and_custom_names() {
        let dir = temp_dir("names");
        fs::create_dir_all(dir.join("docs/sub")).unwrap();
        fs::write(dir.join("docs/a.txt"), b"a").unwrap();
        fs::write(dir.join("docs/sub/b.txt"), b"b").unwrap();

        let entry = |name: &str, is_dir| (name.to_string(), is_dir);
        assert_eq!(names(&[Input::new(dir.join("docs/a.txt"))]), [entry("a.txt", false)]);
        assert_eq!(
            names(&[Input::new(dir.join("docs"))]),
            [entry("docs", true), entry("docs/a.txt", false), entry("docs/sub", true), entry("docs/sub/b.txt", false)]
        );
        assert_eq!(
            names(&[Input::named(dir.join("docs/sub"), "docs/sub")]),
            [entry("docs/sub", true), entry("docs/sub/b.txt", false)]
        );
        // Unnamed folder (e.g. `.`): only its contents.
        assert_eq!(
            names(&[Input::named(dir.join("docs/sub"), "")]),
            [entry("b.txt", false)]
        );
        // Missing inputs are skipped; duplicates written once.
        assert_eq!(
            names(&[Input::new(dir.join("nope")), Input::new(dir.join("docs/a.txt")), Input::new(dir.join("docs/a.txt"))]),
            [entry("a.txt", false)]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    fn expanded(dir: &Path, names: &[&str], recurse: bool) -> (Vec<String>, Vec<String>) {
        let names: Vec<PathBuf> = names.iter().map(|n| PathBuf::from(format!("{}/{n}", dir.display()))).collect();
        let expanded = expand(&names, recurse).unwrap();
        let inputs = expanded.inputs.into_iter().map(|i| i.name).collect();
        let unmatched = expanded
            .unmatched
            .into_iter()
            .map(|u| u.rsplit(['/', '\\']).next().unwrap().to_string())
            .collect();
        (inputs, unmatched)
    }

    #[test]
    fn expands_wildcards_like_7zip() {
        let dir = temp_dir("expand");
        fs::create_dir_all(dir.join("src/sub")).unwrap();
        fs::create_dir_all(dir.join("src/notes.txt.d")).unwrap();
        for file in ["src/a.txt", "src/b.log", "src/sub/c.txt", "src/sub/d.log", "src/README"] {
            fs::write(dir.join(file), b"x").unwrap();
        }
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        // Plain names are added as is.
        assert_eq!(expanded(&dir, &["src/a.txt"], false), (s(&["a.txt"]), s(&[])));
        assert_eq!(expanded(&dir, &["src/a.txt"], true), (s(&["a.txt"]), s(&[])));
        // Wildcards: this folder only, or below it with -r.
        assert_eq!(expanded(&dir, &["src/*.txt"], false), (s(&["a.txt"]), s(&[])));
        assert_eq!(expanded(&dir, &["src/*.txt"], true), (s(&["a.txt", "sub/c.txt"]), s(&[])));
        assert_eq!(expanded(&dir, &["src/?.log"], true), (s(&["b.log", "sub/d.log"]), s(&[])));
        // Folders match too (and bring their contents when written).
        assert_eq!(expanded(&dir, &["src/s*"], false), (s(&["sub"]), s(&[])));
        assert_eq!(expanded(&dir, &["src/*.txt*"], false), (s(&["a.txt", "notes.txt.d"]), s(&[])));
        // Case follows the OS.
        let readme = expanded(&dir, &["src/read*"], false).0;
        assert_eq!(readme.len(), usize::from(cfg!(windows)));
        // Nothing matched.
        assert_eq!(expanded(&dir, &["src/*.zip", "src/missing"], true), (s(&[]), s(&["*.zip", "missing"])));
        // Wildcards only in the last part.
        assert!(expand(&[dir.join("s*").join("a.txt")], false).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
