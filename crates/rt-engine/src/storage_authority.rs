use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStorageRoots {
    roots: Vec<PathBuf>,
}

impl ServerStorageRoots {
    pub fn from_configured_paths(
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, StorageAuthorityError> {
        let mut roots = Vec::new();
        for path in paths {
            let canonical = std::fs::canonicalize(&path).map_err(|source| {
                StorageAuthorityError::InvalidRoot {
                    path: path.clone(),
                    source: source.to_string(),
                }
            })?;
            if !roots.iter().any(|existing| existing == &canonical) {
                roots.push(canonical);
            }
        }
        if roots.is_empty() {
            return Err(StorageAuthorityError::NoConfiguredRoots);
        }
        Ok(Self { roots })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn into_roots(self) -> Vec<PathBuf> {
        self.roots
    }

    /// Authorize a path for storage operations.
    ///
    /// The final path is allowed not to exist yet, but its nearest existing
    /// ancestor must resolve beneath one of the configured canonical roots.
    /// This closes the obvious absolute-path escape and symlink-at-admission
    /// cases. The actual file operation still needs race-resistant, descriptor
    /// relative enforcement before this can be treated as a complete sandbox.
    pub fn authorize_path(&self, path: &Path) -> Result<(), StorageAuthorityError> {
        if !path.is_absolute() {
            return Err(StorageAuthorityError::InvalidPath {
                path: path.to_path_buf(),
                reason: "path must be absolute".to_owned(),
            });
        }
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(StorageAuthorityError::InvalidPath {
                path: path.to_path_buf(),
                reason: "path must not contain parent-directory components".to_owned(),
            });
        }

        let mut existing = path;
        while !existing.exists() {
            existing = existing
                .parent()
                .ok_or_else(|| StorageAuthorityError::InvalidPath {
                    path: path.to_path_buf(),
                    reason: "path has no existing ancestor".to_owned(),
                })?;
        }
        let canonical_existing = std::fs::canonicalize(existing).map_err(|source| {
            StorageAuthorityError::InvalidPath {
                path: path.to_path_buf(),
                reason: format!("canonicalizing existing ancestor: {source}"),
            }
        })?;
        if self
            .roots
            .iter()
            .any(|root| canonical_existing == *root || canonical_existing.starts_with(root))
        {
            Ok(())
        } else {
            Err(StorageAuthorityError::OutsideRoots {
                path: path.to_path_buf(),
                canonical_ancestor: canonical_existing,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageAuthorityError {
    NoConfiguredRoots,
    InvalidRoot {
        path: PathBuf,
        source: String,
    },
    InvalidPath {
        path: PathBuf,
        reason: String,
    },
    OutsideRoots {
        path: PathBuf,
        canonical_ancestor: PathBuf,
    },
}

impl std::fmt::Display for StorageAuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageAuthorityError::NoConfiguredRoots => {
                write!(f, "no configured storage roots are available for execution")
            }
            StorageAuthorityError::InvalidRoot { path, source } => {
                write!(
                    f,
                    "invalid configured storage root {}: {source}",
                    path.display()
                )
            }
            StorageAuthorityError::InvalidPath { path, reason } => {
                write!(f, "invalid storage path {}: {reason}", path.display())
            }
            StorageAuthorityError::OutsideRoots {
                path,
                canonical_ancestor,
            } => write!(
                f,
                "storage path {} resolves outside configured storage roots (ancestor {})",
                path.display(),
                canonical_ancestor.display()
            ),
        }
    }
}

impl std::error::Error for StorageAuthorityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_roots_are_canonicalized_and_deduped() {
        let dir = tempfile::tempdir().unwrap();
        let roots = ServerStorageRoots::from_configured_paths([
            dir.path().to_path_buf(),
            dir.path().join("."),
        ])
        .unwrap();
        assert_eq!(roots.roots().len(), 1);
        assert_eq!(roots.roots()[0], std::fs::canonicalize(dir.path()).unwrap());
    }

    #[test]
    fn empty_roots_are_rejected() {
        assert_eq!(
            ServerStorageRoots::from_configured_paths(Vec::<PathBuf>::new()),
            Err(StorageAuthorityError::NoConfiguredRoots)
        );
    }

    #[test]
    fn missing_roots_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert!(matches!(
            ServerStorageRoots::from_configured_paths([missing]),
            Err(StorageAuthorityError::InvalidRoot { .. })
        ));
    }

    #[test]
    fn existing_and_missing_descendants_are_authorized() {
        let dir = tempfile::tempdir().unwrap();
        let roots = ServerStorageRoots::from_configured_paths([dir.path().to_path_buf()]).unwrap();
        roots.authorize_path(dir.path()).unwrap();
        roots
            .authorize_path(&dir.path().join("new/nested/content"))
            .unwrap();
    }

    #[test]
    fn relative_parent_and_outside_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let roots = ServerStorageRoots::from_configured_paths([dir.path().to_path_buf()]).unwrap();

        assert!(matches!(
            roots.authorize_path(Path::new("relative")),
            Err(StorageAuthorityError::InvalidPath { .. })
        ));
        assert!(matches!(
            roots.authorize_path(&dir.path().join("child/../escape")),
            Err(StorageAuthorityError::InvalidPath { .. })
        ));
        assert!(matches!(
            roots.authorize_path(outside.path()),
            Err(StorageAuthorityError::OutsideRoots { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_existing_path_is_checked_by_canonical_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = root.path().join("link");
        symlink(outside.path(), &link).unwrap();
        let roots = ServerStorageRoots::from_configured_paths([root.path().to_path_buf()]).unwrap();

        assert!(matches!(
            roots.authorize_path(&link.join("payload.bin")),
            Err(StorageAuthorityError::OutsideRoots { .. })
        ));
    }
}
