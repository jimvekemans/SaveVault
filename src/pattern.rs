//! Include / exclude matching for save files.
//!
//! Patterns are relative to a tracked folder and use forward slashes. A pattern matches a
//! file when it names the file itself, names one of its parent folders, or is a glob
//! (`*`, `?`, `**`) that matches either of those. Matching is case-insensitive because
//! most tracked folders live inside Windows prefixes.

use globset::{GlobBuilder, GlobMatcher};

/// Files that are launcher bookkeeping, never save data, wherever they appear.
pub const BUILTIN_EXCLUDES: &[&str] = &[
    "remotecache.vdf",
    "**/remotecache.vdf",
    "steam_autocloud.vdf",
    "**/steam_autocloud.vdf",
    "desktop.ini",
    "**/desktop.ini",
    "Thumbs.db",
    "**/Thumbs.db",
    ".DS_Store",
    "**/.DS_Store",
];

/// Returns true when `s` contains glob metacharacters.
pub fn has_glob_chars(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Normalizes a user / database supplied pattern: forward slashes, no leading or trailing
/// separators, and `[placeholder]` groups (which the save database uses as free-text
/// wildcards rather than character classes) turned into `*`.
pub fn normalize_pattern(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_bracket = false;
    for ch in raw.trim().chars() {
        match ch {
            '\\' => out.push('/'),
            '[' if !in_bracket => {
                in_bracket = true;
                out.push('*');
            }
            ']' if in_bracket => in_bracket = false,
            _ if in_bracket => {}
            _ => out.push(ch),
        }
    }
    out.trim_matches('/').to_string()
}

#[derive(Debug, Clone)]
enum Rule {
    /// Exact path or folder prefix, lowercase.
    Literal(String),
    /// Glob for the path itself plus a glob for anything beneath it.
    Glob(GlobMatcher, GlobMatcher),
}

/// Compiled set of patterns.
#[derive(Debug, Clone, Default)]
pub struct PatternSet {
    rules: Vec<Rule>,
}

impl PatternSet {
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Self {
        let mut rules = Vec::new();
        for raw in patterns {
            let norm = normalize_pattern(raw.as_ref());
            if norm.is_empty() {
                continue;
            }
            if has_glob_chars(&norm) {
                let build = |p: &str| {
                    GlobBuilder::new(p)
                        .case_insensitive(true)
                        .literal_separator(true)
                        .backslash_escape(false)
                        .build()
                        .ok()
                        .map(|g| g.compile_matcher())
                };
                if let (Some(a), Some(b)) = (build(&norm), build(&format!("{norm}/**"))) {
                    rules.push(Rule::Glob(a, b));
                }
            } else {
                rules.push(Rule::Literal(norm.to_lowercase()));
            }
        }
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// `rel` must be a forward-slash relative path.
    pub fn matches(&self, rel: &str) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        let lower = rel.to_lowercase();
        self.rules.iter().any(|rule| match rule {
            Rule::Literal(lit) => {
                lower == *lit
                    || (lower.len() > lit.len()
                        && lower.starts_with(lit.as_str())
                        && lower.as_bytes()[lit.len()] == b'/')
            }
            Rule::Glob(exact, beneath) => exact.is_match(rel) || beneath.is_match(rel),
        })
    }
}

/// Include + exclude filter for one tracked folder.
#[derive(Debug, Clone, Default)]
pub struct PathFilter {
    include: PatternSet,
    exclude: PatternSet,
}

impl PathFilter {
    pub fn new<A: AsRef<str>, B: AsRef<str>>(include: &[A], exclude: &[B]) -> Self {
        Self {
            include: PatternSet::new(include),
            exclude: PatternSet::new(exclude),
        }
    }

    /// True when every file is included (no include patterns).
    pub fn includes_everything(&self) -> bool {
        self.include.is_empty()
    }

    pub fn accepts(&self, rel: &str) -> bool {
        if self.exclude.matches(rel) {
            return false;
        }
        self.include.is_empty() || self.include.matches(rel)
    }
}

/// Matches a single path component against a single-component glob, case-insensitively.
pub fn component_matches(pattern: &str, name: &str) -> bool {
    if !has_glob_chars(pattern) {
        return pattern.eq_ignore_ascii_case(name) || pattern.to_lowercase() == name.to_lowercase();
    }
    GlobBuilder::new(pattern)
        .case_insensitive(true)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
        .map(|g| g.compile_matcher().is_match(name))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_matches_file_and_folder_prefix() {
        let set = PatternSet::new(&["Profiles/slot1.sav", "Characters"]);
        assert!(set.matches("profiles/SLOT1.sav"));
        assert!(set.matches("Characters/Bob/save.das"));
        assert!(!set.matches("CharactersOld/x"));
        assert!(!set.matches("Profiles/slot2.sav"));
    }

    #[test]
    fn globs_match_files_and_everything_beneath_matched_folders() {
        let set = PatternSet::new(&["*.sav", "*/save", "**/*.ess"]);
        assert!(set.matches("quick.SAV"));
        assert!(!set.matches("sub/quick.sav"));
        assert!(set.matches("12345/save/slot0.dat"));
        assert!(set.matches("a/b/c/char.ess"));
    }

    #[test]
    fn database_placeholders_become_wildcards() {
        assert_eq!(normalize_pattern("[store name]/lastsave.txt"), "*/lastsave.txt");
        assert_eq!(normalize_pattern("\\saves\\"), "saves");
        let set = PatternSet::new(&["save[x].txt"]);
        assert!(set.matches("save3.txt"));
    }

    #[test]
    fn filter_applies_excludes_before_includes() {
        let f = PathFilter::new(&["*.sav"], &["autosave*.sav"]);
        assert!(f.accepts("manual.sav"));
        assert!(!f.accepts("autosave_1.sav"));
        assert!(!f.accepts("notes.txt"));
        let all = PathFilter::new::<&str, &str>(&[], &["Cache"]);
        assert!(all.accepts("a/b.dat"));
        assert!(!all.accepts("cache/x.bin"));
    }
}
