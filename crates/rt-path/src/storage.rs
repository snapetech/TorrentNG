use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StorageRootId(pub Uuid);

impl StorageRootId {
    pub fn new() -> Self {
        StorageRootId(Uuid::new_v4())
    }
}

impl Default for StorageRootId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageProfile {
    Hdd,
    Ssd,
    Nvme,
    Network,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageRoot {
    pub id: StorageRootId,
    pub path: PathBuf,
    pub min_free_bytes: u64,
    pub profile: StorageProfile,
}

impl StorageRoot {
    pub fn new(path: PathBuf, profile: StorageProfile) -> Self {
        StorageRoot {
            id: StorageRootId::new(),
            path,
            min_free_bytes: 0,
            profile,
        }
    }
}
