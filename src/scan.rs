//! The coverage checker: the write styles that skip SeaORM's lifecycle hooks.
//!
//! SeaORM fires `before_save` / `after_save` / `before_delete` / `after_delete`
//! only from `ActiveModel`'s own methods. Repository-style writes run the SQL
//! directly and never touch the hooks, so an audited table written that way
//! leaves no audit trail. The rule is therefore short:
//!
//! > Audited tables are written only with ActiveModel methods (`am.insert`,
//! > `am.update`, `am.save`, `am.delete`), inside a transaction. Never
//! > repository style. For the bulk methods, use the crate's audited wrappers
//! > ([`crate::bulk`]), not the raw ones.
//!
//! Leaving that rule as prose is how it gets broken, so this module is the
//! enforcement. [`Checker`] finds the forbidden call sites and the
//! `seaorm-auto-audit-check` binary fails the build when it finds one.
//!
//! | fires hooks                        | does not fire hooks                |
//! |------------------------------------|------------------------------------|
//! | `am.insert(db)`                    | `Entity::insert(am).exec(db)`      |
//! | `am.update(db)`                    | `Entity::update(am).filter(..)`    |
//! | `am.save(db)`                      | `Entity::insert_many(..)`          |
//! | `am.delete(db)`                    | `Entity::update_many()`            |
//! | nested `builder().save()`          | `Entity::delete_many()`            |
//! |                                    | `Entity::delete_by_id`             |
//! |                                    | `update_without_returning`         |
//!
//! `update_without_returning` is the odd one in the right-hand column: it
//! runs `before_save` but not `after_save`, so the write still lands
//! with no audit row.
//!
//! Comments and string literals are blanked before matching, so documenting the
//! forbidden styles — as the table above does — does not trip the check.

use std::path::{Path, PathBuf};

/// The write styles that skip SeaORM's lifecycle hooks.
pub const FORBIDDEN_PATTERNS: &[&str] = &[
    "Entity::insert(",
    "Entity::insert_many",
    "Entity::update(",
    "Entity::update_many",
    "Entity::delete_many",
    "Entity::delete_by_id",
    "update_without_returning",
];

/// One forbidden call site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// The file the call site is in.
    pub path: PathBuf,
    /// The line the call site is on, counted from 1.
    pub line: usize,
    /// The pattern that matched.
    pub pattern: &'static str,
    /// The text of the offending line.
    pub text: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: `{}` skips the audit hooks — {}",
            self.path.display(),
            self.line,
            self.pattern,
            self.text.trim()
        )
    }
}

/// Why a scan could not produce a trustworthy answer.
///
/// A checker that cannot read its input must not report "clean": a scan
/// that silently skipped files, or found nothing to scan at all, would
/// pass a build gate while checking nothing. Both failures are errors,
/// never empty results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanError {
    /// A path in the tree could not be read.
    Unreadable {
        /// The file or directory that could not be read.
        path: PathBuf,
        /// The operating system's description of the failure.
        message: String,
    },
    /// The root held no Rust source files, so nothing was scanned.
    NothingScanned {
        /// The root that yielded no source files.
        path: PathBuf,
    },
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::Unreadable { path, message } => {
                write!(f, "could not read `{}`: {message}", path.display())
            }
            ScanError::NothingScanned { path } => write!(
                f,
                "`{}` contains no Rust source files; nothing was scanned",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ScanError {}

/// Finds write styles that skip the audit hooks.
#[derive(Clone, Debug)]
pub struct Checker {
    patterns: Vec<&'static str>,
    allowlist: Vec<PathBuf>,
}

impl Default for Checker {
    fn default() -> Self {
        Self::new()
    }
}

impl Checker {
    /// A checker for the default patterns and no allowlist.
    pub fn new() -> Self {
        Self {
            patterns: FORBIDDEN_PATTERNS.to_vec(),
            allowlist: Vec::new(),
        }
    }

    /// Exempt a path from the check.
    ///
    /// A path may be a directory (everything under it is exempt), a file, or a
    /// bare file name such as `audit.rs` (every file with that name is
    /// exempt).
    pub fn allow(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.allowlist.push(path.into());
        self
    }

    /// Replace the patterns this checker looks for.
    pub fn patterns(&mut self, patterns: &[&'static str]) -> &mut Self {
        self.patterns = patterns.to_vec();
        self
    }

    /// The paths this checker exempts.
    pub fn allowlist(&self) -> &[PathBuf] {
        &self.allowlist
    }

    /// The patterns this checker looks for.
    pub fn forbidden_patterns(&self) -> &[&'static str] {
        &self.patterns
    }

    /// Scan `root` for forbidden call sites.
    ///
    /// `root` may be a file or a directory. Directories are walked
    /// recursively; `target`, `.git` and other hidden directories are skipped.
    ///
    /// # Errors
    ///
    /// [`ScanError::Unreadable`] when any part of the tree cannot be
    /// read, and [`ScanError::NothingScanned`] when the root holds no
    /// Rust source files at all. A scan that checked nothing is an
    /// error, never an empty result: this checker's job is gating, and
    /// a gate that passes on no evidence is worse than no gate.
    pub fn scan(&self, root: &Path) -> Result<Vec<Violation>, ScanError> {
        let files = source_files(root)?;
        if files.is_empty() {
            return Err(ScanError::NothingScanned {
                path: root.to_path_buf(),
            });
        }
        let mut violations = Vec::new();
        for path in files {
            if self.is_allowed(&path) {
                continue;
            }
            let contents =
                std::fs::read_to_string(&path).map_err(|error| ScanError::Unreadable {
                    path: path.clone(),
                    message: error.to_string(),
                })?;
            violations.extend(self.scan_source(&path, &contents));
        }
        Ok(violations)
    }

    /// Scan one source file for forbidden call sites.
    pub fn scan_source(&self, path: &Path, source: &str) -> Vec<Violation> {
        let code = strip_comments_and_strings(source);
        let mut violations = Vec::new();
        for (index, (line, original)) in code.lines().zip(source.lines()).enumerate() {
            for pattern in &self.patterns {
                if line.contains(pattern) {
                    violations.push(Violation {
                        path: path.to_path_buf(),
                        line: index + 1,
                        pattern,
                        text: original.to_owned(),
                    });
                }
            }
        }
        violations
    }

    /// Whether `path` is exempt.
    pub fn is_allowed(&self, path: &Path) -> bool {
        self.allowlist.iter().any(|entry| {
            if path == entry {
                return true;
            }
            if path.starts_with(entry) {
                return true;
            }
            // A bare file name exempts every file with that name.
            entry
                .parent()
                .is_none_or(|parent| parent.as_os_str().is_empty())
                && entry.file_name() == path.file_name()
        })
    }
}

/// The Rust source files under `root`.
///
/// A directory that cannot be listed, or an entry that cannot be read,
/// is an error, not an empty result: silently dropping part of the
/// tree is how a forbidden write hides from the gate.
fn source_files(root: &Path) -> Result<Vec<PathBuf>, ScanError> {
    if root.is_file() {
        return Ok(
            if root.extension().is_some_and(|extension| extension == "rs") {
                vec![root.to_path_buf()]
            } else {
                Vec::new()
            },
        );
    }

    let mut files = Vec::new();
    let entries = std::fs::read_dir(root).map_err(|error| ScanError::Unreadable {
        path: root.to_path_buf(),
        message: error.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| ScanError::Unreadable {
            path: root.to_path_buf(),
            message: error.to_string(),
        })?;
        // The entry's own type, which does not follow symlinks. A symlinked
        // directory is skipped rather than walked: a link pointing back at
        // an ancestor would otherwise recurse until the stack overflows,
        // and a link out of the tree would scan files nobody named.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_dir() {
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            files.extend(source_files(&path)?);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Blank out comments and string literals, keeping the line structure.
///
/// The result is the source with everything that cannot be a call site
/// replaced by spaces, so a pattern match on it can only come from real code.
/// Line numbers are preserved, because the blanked text keeps every newline.
fn strip_comments_and_strings(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    let mut block_depth = 0usize;

    while index < chars.len() {
        let current = chars[index];
        let next = chars.get(index + 1).copied();

        if block_depth > 0 {
            if current == '/' && next == Some('*') {
                block_depth += 1;
                index += 2;
                continue;
            }
            if current == '*' && next == Some('/') {
                block_depth -= 1;
                index += 2;
                continue;
            }
            if current == '\n' {
                out.push('\n');
            }
            index += 1;
            continue;
        }

        // Line comment.
        if current == '/' && next == Some('/') {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }

        // Block comment.
        if current == '/' && next == Some('*') {
            block_depth = 1;
            index += 2;
            continue;
        }

        // Raw string: `r"..."`, `r#"..."#`, `r##"..."##`.
        if current == 'r'
            && let Some(hashes) = raw_string_hashes(&chars, index)
        {
            let start = index + 1 + hashes;
            let end = raw_string_end(&chars, start, hashes);
            out.push_str(&blank(&chars[index..end]));
            index = end;
            continue;
        }

        // Normal string.
        if current == '"' {
            let end = string_end(&chars, index);
            out.push_str(&blank(&chars[index..end]));
            index = end;
            continue;
        }

        // Byte string: `b"..."`.
        if current == 'b' && next == Some('"') {
            let end = string_end(&chars, index + 1);
            out.push_str(&blank(&chars[index..end]));
            index = end;
            continue;
        }

        // Character literal, but not a lifetime (`'a`).
        if current == '\''
            && let Some(end) = char_literal_end(&chars, index)
        {
            out.push_str(&blank(&chars[index..end]));
            index = end;
            continue;
        }

        out.push(current);
        index += 1;
    }

    out
}

/// The number of `#` in a raw string prefix, if `index` starts one.
fn raw_string_hashes(chars: &[char], index: usize) -> Option<usize> {
    let mut hashes = 0;
    while chars.get(index + 1 + hashes) == Some(&'#') {
        hashes += 1;
    }
    if chars.get(index + 1 + hashes) == Some(&'"') {
        Some(hashes)
    } else {
        None
    }
}

/// The index just past the end of a raw string that starts at `start`.
fn raw_string_end(chars: &[char], start: usize, hashes: usize) -> usize {
    let mut index = start;
    while index < chars.len() {
        if chars[index] == '"' {
            let mut closing = 0;
            while closing < hashes && chars.get(index + 1 + closing) == Some(&'#') {
                closing += 1;
            }
            if closing == hashes {
                return index + 1 + hashes;
            }
        }
        index += 1;
    }
    chars.len()
}

/// The index just past the end of a normal string that starts at `start`.
fn string_end(chars: &[char], start: usize) -> usize {
    let mut index = start + 1;
    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            '"' => return index + 1,
            '\n' => return index + 1,
            _ => index += 1,
        }
    }
    chars.len()
}

/// The index just past the end of a character literal, if `start` starts one.
fn char_literal_end(chars: &[char], start: usize) -> Option<usize> {
    match chars.get(start + 1) {
        Some('\\') => {
            // `'\\'`, `'\n'`, ...
            if chars.get(start + 3) == Some(&'\'') {
                Some(start + 4)
            } else {
                None
            }
        }
        Some(_) => {
            if chars.get(start + 2) == Some(&'\'') {
                Some(start + 3)
            } else {
                None
            }
        }
        None => None,
    }
}

/// Replace every character but the newlines with a space.
fn blank(chars: &[char]) -> String {
    chars
        .iter()
        .map(|character| if *character == '\n' { '\n' } else { ' ' })
        .collect()
}
