use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStorageRoots {
    roots: Vec<PathBuf>,
}

impl ServerStorageRoots {
    pub fn from_configured_paths(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self, StorageAuthorityError> {
        let mut roots = Vec::new();
        for path in paths {
            let canonical = std::fs::canonicalize(&path).map_err(|source| StorageAuthorityError::InvalidRoot {
                path: path.clone(),
                source: source.to_string(),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageAuthorityError {
    NoConfiguredRoots,
    InvalidRoot { path: PathBuf, source: String },
}

impl std::fmt::Display for StorageAuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageAuthorityError::NoConfiguredRoots => write!(f, "no configured storage roots are available for execution"),
            StorageAuthorityError::InvalidRoot { path, source } => {
                write!(f, "invalid configured storage root {}: {source}", path.display())
            }
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
            dir.path().join(".")
        ]).unwrap();
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
}
