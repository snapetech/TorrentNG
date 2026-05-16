use std::{collections::BTreeMap, collections::BTreeSet, sync::Arc};
use tokio::sync::RwLock;

use rt_engine::EngineHandle;
use rt_session::SessionRegistry;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub categories: Arc<RwLock<BTreeMap<String, String>>>,
    pub tags: Arc<RwLock<BTreeSet<String>>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            engine: None,
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
        }
    }

    pub fn with_registry(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        AppState {
            registry,
            engine: None,
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        AppState {
            registry,
            engine: Some(engine),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
