//! Selecting archive entries (and files to add) by name, with 7-Zip style
//! wildcards.

/// Which entries of an archive an operation applies to.
///
/// A rule selects the entry with that path and, when it names a directory,
/// everything below it: `docs` selects `docs`, `docs/` and `docs/a.txt`, but
/// not `docs2/a.txt`. `/` and `\` are equivalent, and leading `./` or `/` and
/// trailing separators are ignored on both sides. A rule naming the root (`.`
/// or `/`), or no rule at all, selects everything.
///
/// Rules built with [`EntryFilter::patterns`] may use `*` (any run of
/// characters within one path segment) and `?` (one character), and match
/// case-insensitively on Windows, like 7-Zip and the file system. With
/// `recurse` (`-r`), a rule with wildcards also matches below the root:
/// `*.txt` selects `a.txt` and `docs/b.txt`, and `sub/*.txt` selects
/// `x/sub/a.txt`. A rule without wildcards is always an exact path.
#[derive(Debug, Clone, Default)]
pub struct EntryFilter {
    /// Each rule split into path segments, and whether it may match below
    /// the root.
    rules: Vec<(Vec<String>, bool)>,
    wildcards: bool,
    ignore_case: bool,
}

impl EntryFilter {
    /// Selects these exact entry paths (and the contents of directories among
    /// them). No wildcards, case-sensitive: used for names picked in the GUI,
    /// which may legitimately contain `*` or differ only by case.
    pub fn paths<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            rules: paths.into_iter().map(|path| (segments(path.as_ref()), false)).collect(),
            wildcards: false,
            ignore_case: false,
        }
    }

    /// Selects entries matching these command-line patterns (see the type
    /// documentation); `recurse` is `-r`.
    pub fn patterns<I, S>(patterns: I, recurse: bool) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            rules: patterns
                .into_iter()
                .map(|pattern| (segments(pattern.as_ref()), recurse && has_wildcard(pattern.as_ref())))
                .collect(),
            wildcards: true,
            ignore_case: cfg!(windows),
        }
    }

    /// Whether this filter selects everything.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty() || self.rules.iter().any(|(rule, _)| rule.is_empty())
    }

    /// Whether the entry named `name` is selected.
    pub fn matches(&self, name: &str) -> bool {
        if self.is_empty() {
            return true;
        }
        let name = segments(name);
        self.rules.iter().any(|(rule, recurse)| {
            if rule.len() > name.len() {
                return false;
            }
            let last_start = if *recurse { name.len() - rule.len() } else { 0 };
            (0..=last_start).any(|start| {
                rule.iter()
                    .zip(&name[start..])
                    .all(|(pattern, segment)| self.segment_matches(pattern, segment))
            })
        })
    }

    fn segment_matches(&self, pattern: &str, segment: &str) -> bool {
        match (self.wildcards, self.ignore_case) {
            (false, false) => pattern == segment,
            (false, true) => pattern.to_lowercase() == segment.to_lowercase(),
            (true, false) => wildcard_match(pattern, segment),
            (true, true) => wildcard_match(&pattern.to_lowercase(), &segment.to_lowercase()),
        }
    }
}

/// Whether an optional filter selects `name` (no filter selects everything).
pub(crate) fn selected(filter: Option<&EntryFilter>, name: &str) -> bool {
    filter.is_none_or(|filter| filter.matches(name))
}

/// Whether `name` contains a wildcard character (`*` or `?`).
pub fn has_wildcard(name: &str) -> bool {
    name.contains(['*', '?'])
}

/// Matches one path segment against a pattern with `*` and `?`.
pub(crate) fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    // Position of the last `*` in the pattern, and of the text it was tried at.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                // Let the last `*` swallow one more character and retry.
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// Splits a path into its segments, ignoring empty and `.` segments.
fn segments(path: &str) -> Vec<String> {
    path.split(['/', '\\'])
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(rules: &[&str], name: &str) -> bool {
        EntryFilter::paths(rules).matches(name)
    }

    fn pattern(rules: &[&str], recurse: bool, name: &str) -> bool {
        EntryFilter::patterns(rules, recurse).matches(name)
    }

    #[test]
    fn exact_paths_only() {
        assert!(exact(&["a.txt"], "a.txt"));
        assert!(!exact(&["a.txt"], "data.txt"));
        assert!(!exact(&["a.txt"], "a.txt.bak"));
        assert!(!exact(&["a.txt"], "dir/a.txt"));
        assert!(!exact(&["dir/a.txt"], "a.txt"));
        assert!(!exact(&["A.txt"], "a.txt"));
        // Exact rules take `*` literally.
        assert!(exact(&["a*.txt"], "a*.txt"));
        assert!(!exact(&["a*.txt"], "ab.txt"));
    }

    #[test]
    fn selects_directory_contents() {
        assert!(exact(&["docs"], "docs"));
        assert!(exact(&["docs"], "docs/"));
        assert!(exact(&["docs"], "docs/a.txt"));
        assert!(exact(&["docs/"], "docs/sub/b.txt"));
        assert!(!exact(&["docs"], "docs2/a.txt"));
        assert!(!exact(&["docs"], "old/docs/a.txt"));
        assert!(!exact(&["docs/sub"], "docs/a.txt"));
        assert!(pattern(&["do*"], false, "docs/sub/b.txt"));
    }

    #[test]
    fn normalizes_separators_and_prefixes() {
        assert!(exact(&["docs\\a.txt"], "docs/a.txt"));
        assert!(exact(&["docs/a.txt"], "docs\\a.txt"));
        assert!(exact(&["./docs/a.txt"], "docs/a.txt"));
        assert!(exact(&["docs/a.txt"], "./docs/a.txt"));
        assert!(exact(&["/docs"], "docs/a.txt"));
        assert!(exact(&[".hidden"], ".hidden"));
        assert!(!exact(&[".hidden"], "hidden"));
    }

    #[test]
    fn any_of_several_and_empty_cases() {
        assert!(exact(&["x", "docs"], "docs/a.txt"));
        assert!(!exact(&["x", "y"], "docs/a.txt"));
        assert!(EntryFilter::default().matches("anything"));
        assert!(EntryFilter::paths(Vec::<String>::new()).matches("anything"));
        assert!(exact(&["."], "docs/a.txt"));
        assert!(exact(&["/"], "docs/a.txt"));
        assert!(pattern(&["*"], false, "docs/a.txt"));
    }

    #[test]
    fn wildcards_match_within_one_segment() {
        assert!(wildcard_match("*.txt", "a.txt"));
        assert!(wildcard_match("*.txt", ".txt"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(wildcard_match("*a*b*", "xxaxxbxx"));
        assert!(!wildcard_match("*a*b", "xxaxxbxxc"));
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("**", "abc"));
        assert!(!wildcard_match("*.txt", "a.txt.bak"));
        assert!(wildcard_match("é*", "été"));

        assert!(pattern(&["*.txt"], false, "a.txt"));
        assert!(!pattern(&["*.txt"], false, "docs/a.txt"));
        assert!(pattern(&["docs/*.txt"], false, "docs/a.txt"));
        assert!(pattern(&["*/a.txt"], false, "docs/a.txt"));
        assert!(!pattern(&["docs/*.txt"], false, "docs/sub/a.txt"));
    }

    #[test]
    fn recurse_matches_below_the_root() {
        assert!(pattern(&["*.txt"], true, "a.txt"));
        assert!(pattern(&["*.txt"], true, "docs/sub/a.txt"));
        assert!(pattern(&["sub/*.txt"], true, "docs/sub/a.txt"));
        assert!(!pattern(&["*.txt"], true, "docs/a.log"));
        // A matched directory still brings its contents.
        assert!(pattern(&["su?"], true, "docs/sub/a.log"));
        // Names without wildcards stay exact paths.
        assert!(!pattern(&["readme"], true, "x/readme"));
        assert!(pattern(&["readme"], true, "readme"));
    }

    #[test]
    fn case_follows_the_os_for_patterns() {
        assert_eq!(pattern(&["*.TXT"], false, "a.txt"), cfg!(windows));
        assert_eq!(pattern(&["Docs"], false, "docs/a.txt"), cfg!(windows));
        assert!(!exact(&["Docs"], "docs/a.txt"));
    }

    #[test]
    fn detects_wildcards() {
        assert!(has_wildcard("*.txt"));
        assert!(has_wildcard("a?.txt"));
        assert!(!has_wildcard("docs/a.txt"));
    }
}
