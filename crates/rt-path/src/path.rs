use std::path::{Path, PathBuf};

use crate::error::PathError;

// Windows reserved names (case-insensitive, with or without extension)
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A validated, sanitized, storage-root-relative path.
///
/// Invariants guaranteed by construction:
/// - No absolute components
/// - No `..` components
/// - No empty components
/// - No NUL bytes
/// - No Windows-reserved names (when check_windows_reserved is true)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SafeRelPath(Vec<String>);

impl SafeRelPath {
    /// Parse a sequence of torrent path components into a `SafeRelPath`.
    pub fn from_components(
        components: &[impl AsRef<str>],
        check_windows_reserved: bool,
    ) -> Result<Self, PathError> {
        if components.is_empty() {
            return Err(PathError::EmptyPath);
        }
        let mut parts = Vec::with_capacity(components.len());
        for c in components {
            let s = c.as_ref();
            validate_component(s, check_windows_reserved)?;
            parts.push(s.to_owned());
        }
        Ok(SafeRelPath(parts))
    }

    /// Parse a single-component name (single-file torrent).
    pub fn from_name(
        name: impl AsRef<str>,
        check_windows_reserved: bool,
    ) -> Result<Self, PathError> {
        Self::from_components(&[name.as_ref()], check_windows_reserved)
    }

    pub fn components(&self) -> &[String] {
        &self.0
    }

    /// Resolve this path against a storage root, returning an absolute OS path.
    pub fn resolve(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        for c in &self.0 {
            p.push(c);
        }
        p
    }

    pub fn as_display(&self) -> String {
        self.0.join("/")
    }
}

fn validate_component(s: &str, check_windows_reserved: bool) -> Result<(), PathError> {
    if s.is_empty() {
        return Err(PathError::EmptyComponent);
    }
    if s.contains('\0') {
        return Err(PathError::NulByte);
    }
    if s == ".." {
        return Err(PathError::ParentTraversal(s.to_owned()));
    }
    if s.starts_with('/') || s.starts_with('\\') {
        return Err(PathError::AbsolutePath(s.to_owned()));
    }
    // Reject any component that would be treated as absolute on any platform
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        return Err(PathError::AbsolutePath(s.to_owned()));
    }
    if check_windows_reserved {
        let stem = s.split('.').next().unwrap_or(s);
        if WINDOWS_RESERVED
            .iter()
            .any(|&r| r.eq_ignore_ascii_case(stem))
        {
            return Err(PathError::WindowsReservedName(s.to_owned()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_simple_path() {
        let p = SafeRelPath::from_components(&["foo", "bar.txt"], false).unwrap();
        assert_eq!(p.as_display(), "foo/bar.txt");
    }

    #[test]
    fn reject_parent_traversal() {
        assert_eq!(
            SafeRelPath::from_components(&["foo", "..", "etc"], false).unwrap_err(),
            PathError::ParentTraversal("..".into())
        );
    }

    #[test]
    fn reject_absolute_unix() {
        assert!(SafeRelPath::from_components(&["/etc/passwd"], false).is_err());
    }

    #[test]
    fn reject_absolute_windows_drive() {
        assert!(SafeRelPath::from_components(&["C:evil"], false).is_err());
    }

    #[test]
    fn reject_nul_byte() {
        assert_eq!(
            SafeRelPath::from_components(&["foo\0bar"], false).unwrap_err(),
            PathError::NulByte
        );
    }

    #[test]
    fn reject_empty_component() {
        assert_eq!(
            SafeRelPath::from_components(&["foo", ""], false).unwrap_err(),
            PathError::EmptyComponent
        );
    }

    #[test]
    fn reject_empty_path() {
        let empty: &[&str] = &[];
        assert_eq!(
            SafeRelPath::from_components(empty, false).unwrap_err(),
            PathError::EmptyPath
        );
    }

    #[test]
    fn reject_windows_reserved_nul() {
        assert!(SafeRelPath::from_components(&["NUL"], true).is_err());
        assert!(SafeRelPath::from_components(&["nul.txt"], true).is_err());
        assert!(SafeRelPath::from_components(&["COM1"], true).is_err());
    }

    #[test]
    fn windows_reserved_allowed_when_disabled() {
        assert!(SafeRelPath::from_components(&["NUL"], false).is_ok());
    }

    #[test]
    fn resolve_against_root() {
        let p = SafeRelPath::from_components(&["data", "file.txt"], false).unwrap();
        let resolved = p.resolve(Path::new("/storage"));
        assert_eq!(resolved, PathBuf::from("/storage/data/file.txt"));
    }
}
