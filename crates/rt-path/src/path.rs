use std::path::{Path, PathBuf};

use crate::error::PathError;

// Windows reserved names (case-insensitive, with or without extension)
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "COM¹", "COM²", "COM³", "CONIN$", "CONOUT$", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5",
    "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];
const MAX_COMPONENT_BYTES: usize = 4096;
pub const MAX_COMPONENTS: usize = 256;
pub const MAX_PATH_BYTES: usize = MAX_COMPONENT_BYTES * MAX_COMPONENTS + MAX_COMPONENTS - 1;

/// A validated, sanitized, storage-root-relative path.
///
/// Invariants guaranteed by construction:
/// - No absolute components
/// - No `..` components
/// - No empty components
/// - No NUL bytes
/// - No Win32-invalid, reserved, or normalization-ambiguous names (when
///   `check_windows_reserved` is true)
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
        if components.len() > MAX_COMPONENTS {
            return Err(PathError::TooManyComponents {
                count: components.len(),
                max: MAX_COMPONENTS,
            });
        }
        let mut parts = Vec::with_capacity(components.len());
        let mut path_bytes: usize = 0;
        for c in components {
            let s = c.as_ref();
            validate_component(s, check_windows_reserved)?;
            path_bytes = path_bytes.saturating_add(s.len());
            if !parts.is_empty() {
                path_bytes = path_bytes.saturating_add(1);
            }
            if path_bytes > MAX_PATH_BYTES {
                return Err(PathError::PathTooLong {
                    len: path_bytes,
                    max: MAX_PATH_BYTES,
                });
            }
            parts.push(s.to_owned());
        }
        Ok(SafeRelPath(parts))
    }

    /// Parse a slash-separated path while bounding the number of borrowed
    /// components before allocating the validated path.
    pub fn from_slash_separated(
        path: &str,
        check_windows_reserved: bool,
    ) -> Result<Self, PathError> {
        if path.len() > MAX_PATH_BYTES {
            return Err(PathError::PathTooLong {
                len: path.len(),
                max: MAX_PATH_BYTES,
            });
        }
        let mut components = Vec::new();
        for component in path.split('/') {
            if components.len() >= MAX_COMPONENTS {
                return Err(PathError::TooManyComponents {
                    count: components.len() + 1,
                    max: MAX_COMPONENTS,
                });
            }
            components.push(component);
        }
        Self::from_components(&components, check_windows_reserved)
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

/// Reject duplicate file paths and paths that would require one file to also
/// be another file's parent directory. Windows comparisons use the operating
/// system's ordinal, case-insensitive table so validation matches ordinary
/// Win32 path lookup for Unicode as well as ASCII names.
pub fn validate_unique_file_paths<'a>(
    paths: impl IntoIterator<Item = &'a SafeRelPath>,
) -> Result<(), PathError> {
    let mut comparable = paths
        .into_iter()
        .map(ComparablePath::new)
        .collect::<Vec<_>>();
    comparable.sort_unstable_by(ComparablePath::compare);

    for pair in comparable.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if previous.compare(current).is_eq() || previous.is_directory_prefix_of(current) {
            return Err(PathError::ConflictingPaths(current.path.as_display()));
        }
    }
    Ok(())
}

struct ComparablePath<'a> {
    path: &'a SafeRelPath,
    #[cfg(windows)]
    components: Vec<Vec<u16>>,
}

impl<'a> ComparablePath<'a> {
    fn new(path: &'a SafeRelPath) -> Self {
        Self {
            path,
            #[cfg(windows)]
            components: path
                .components()
                .iter()
                .map(|component| component.encode_utf16().collect())
                .collect(),
        }
    }

    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        #[cfg(windows)]
        {
            for (left, right) in self.components.iter().zip(&other.components) {
                let ordering = compare_windows_ordinal_ignore_case(left, right);
                if !ordering.is_eq() {
                    return ordering;
                }
            }
            self.components.len().cmp(&other.components.len())
        }
        #[cfg(not(windows))]
        {
            for (left, right) in self.path.components().iter().zip(other.path.components()) {
                let ordering = left.cmp(right);
                if !ordering.is_eq() {
                    return ordering;
                }
            }
            self.path
                .components()
                .len()
                .cmp(&other.path.components().len())
        }
    }

    fn is_directory_prefix_of(&self, other: &Self) -> bool {
        let self_len = self.path.components().len();
        let other_len = other.path.components().len();
        self_len < other_len
            && (0..self_len).all(|index| {
                #[cfg(windows)]
                {
                    compare_windows_ordinal_ignore_case(
                        &self.components[index],
                        &other.components[index],
                    )
                    .is_eq()
                }
                #[cfg(not(windows))]
                {
                    self.path.components()[index] == other.path.components()[index]
                }
            })
    }
}

#[cfg(windows)]
fn compare_windows_ordinal_ignore_case(left: &[u16], right: &[u16]) -> std::cmp::Ordering {
    use windows_sys::Win32::Globalization::{
        CompareStringOrdinal, CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN,
    };

    // Every component is bounded by MAX_COMPONENT_BYTES before it reaches
    // this function, so the UTF-16 lengths fit in i32.
    let result = unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    };
    match result {
        CSTR_LESS_THAN => std::cmp::Ordering::Less,
        CSTR_EQUAL => std::cmp::Ordering::Equal,
        CSTR_GREATER_THAN => std::cmp::Ordering::Greater,
        // Failure must not let two paths be accepted as distinct.
        _ => std::cmp::Ordering::Equal,
    }
}

fn validate_component(s: &str, check_windows_reserved: bool) -> Result<(), PathError> {
    if s.is_empty() {
        return Err(PathError::EmptyComponent);
    }
    if s.len() > MAX_COMPONENT_BYTES {
        return Err(PathError::ComponentTooLong {
            len: s.len(),
            max: MAX_COMPONENT_BYTES,
        });
    }
    if s.contains('\0') {
        return Err(PathError::NulByte);
    }
    if s == ".." {
        return Err(PathError::ParentTraversal(s.to_owned()));
    }
    if s == "." {
        return Err(PathError::IllegalCharacter('.'));
    }
    if let Some(separator) = s
        .chars()
        .find(|character| *character == '/' || *character == '\\')
    {
        return Err(PathError::IllegalCharacter(separator));
    }
    // Reject any component that would be treated as absolute on any platform
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        return Err(PathError::AbsolutePath(s.to_owned()));
    }
    if check_windows_reserved {
        if s.starts_with(' ') || s.ends_with([' ', '.']) {
            let character = if s.starts_with(' ') {
                ' '
            } else {
                s.chars().next_back().unwrap_or(' ')
            };
            return Err(PathError::IllegalCharacter(character));
        }
        if let Some(character) = s.chars().find(|character| {
            matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
                || ('\u{1}'..='\u{1f}').contains(character)
        }) {
            return Err(PathError::IllegalCharacter(character));
        }
        let stem = s
            .split('.')
            .next()
            .unwrap_or(s)
            .trim_end_matches([' ', '.']);
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
    fn reject_embedded_separators_and_current_directory() {
        assert!(SafeRelPath::from_components(&["nested/file"], false).is_err());
        assert!(SafeRelPath::from_components(&["nested\\file"], false).is_err());
        assert!(SafeRelPath::from_components(&["."], false).is_err());
    }

    #[test]
    fn reject_oversized_component() {
        let component = "x".repeat(MAX_COMPONENT_BYTES + 1);
        assert!(matches!(
            SafeRelPath::from_components(&[component], false),
            Err(PathError::ComponentTooLong { .. })
        ));
    }

    #[test]
    fn reject_too_many_components_before_copying_them() {
        let components = vec!["x"; MAX_COMPONENTS + 1];
        assert_eq!(
            SafeRelPath::from_components(&components, false).unwrap_err(),
            PathError::TooManyComponents {
                count: MAX_COMPONENTS + 1,
                max: MAX_COMPONENTS,
            }
        );
    }

    #[test]
    fn slash_parser_bounds_component_collection() {
        let path = std::iter::repeat_n("x", MAX_COMPONENTS + 1)
            .collect::<Vec<_>>()
            .join("/");
        assert!(matches!(
            SafeRelPath::from_slash_separated(&path, false),
            Err(PathError::TooManyComponents { .. })
        ));
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
    fn windows_mode_rejects_streams_invalid_characters_and_normalized_names() {
        for component in [
            "payload:stream",
            "bad<name",
            "bad\u{1f}name",
            "name.",
            "name ",
            " leading-space",
            "CON .txt",
            "COM¹.log",
            "LPT³",
            "CONIN$",
            "CONOUT$.txt",
        ] {
            assert!(
                SafeRelPath::from_name(component, true).is_err(),
                "Windows-compatible paths must reject {component:?}"
            );
        }
        for component in [".hidden", "name with spaces", "COM0", "payload"] {
            assert!(
                SafeRelPath::from_name(component, true).is_ok(),
                "Windows-compatible paths should retain {component:?}"
            );
        }
    }

    #[test]
    fn file_path_set_rejects_duplicate_and_file_directory_conflicts() {
        let duplicate_a = SafeRelPath::from_name("payload.bin", false).unwrap();
        let duplicate_b = SafeRelPath::from_name("payload.bin", false).unwrap();
        let parent = SafeRelPath::from_name("payload", false).unwrap();
        let child = SafeRelPath::from_components(&["payload", "data.bin"], false).unwrap();
        let sibling_a = SafeRelPath::from_components(&["payload", "a.bin"], false).unwrap();
        let sibling_b = SafeRelPath::from_components(&["payload", "b.bin"], false).unwrap();

        assert!(matches!(
            validate_unique_file_paths([&duplicate_a, &duplicate_b]),
            Err(PathError::ConflictingPaths(_))
        ));
        assert!(matches!(
            validate_unique_file_paths([&parent, &child]),
            Err(PathError::ConflictingPaths(_))
        ));
        assert!(validate_unique_file_paths([&sibling_a, &sibling_b]).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_set_rejects_ascii_case_aliases() {
        let uppercase = SafeRelPath::from_name("Payload.bin", true).unwrap();
        let lowercase = SafeRelPath::from_name("payload.BIN", true).unwrap();
        assert!(matches!(
            validate_unique_file_paths([&uppercase, &lowercase]),
            Err(PathError::ConflictingPaths(_))
        ));
    }

    #[test]
    fn resolve_against_root() {
        let p = SafeRelPath::from_components(&["data", "file.txt"], false).unwrap();
        let resolved = p.resolve(Path::new("/storage"));
        assert_eq!(resolved, PathBuf::from("/storage/data/file.txt"));
    }
}
