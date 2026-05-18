use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};
use tokio::sync::RwLock;

use rt_engine::{EngineGlobalLimits, EngineHandle};
use rt_session::SessionRegistry;

pub type JsonMap = serde_json::Map<String, serde_json::Value>;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub categories: Arc<RwLock<BTreeMap<String, String>>>,
    pub tags: Arc<RwLock<BTreeSet<String>>>,
    pub tracker_projection_cache: Arc<RwLock<HashMap<String, (String, u32)>>>,
    pub preference_overrides: Arc<RwLock<JsonMap>>,
    pub global_limits: Arc<RwLock<EngineGlobalLimits>>,
    pub search_plugins: Arc<RwLock<JsonMap>>,
    pub search_jobs: Arc<RwLock<JsonMap>>,
    pub next_search_id: Arc<RwLock<i64>>,
    pub rss_items: Arc<RwLock<JsonMap>>,
    pub rss_rules: Arc<RwLock<JsonMap>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            engine: None,
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
            tracker_projection_cache: Arc::new(RwLock::new(HashMap::new())),
            preference_overrides: Arc::new(RwLock::new(serde_json::Map::new())),
            global_limits: Arc::new(RwLock::new(EngineGlobalLimits::default())),
            search_plugins: Arc::new(RwLock::new(serde_json::Map::new())),
            search_jobs: Arc::new(RwLock::new(serde_json::Map::new())),
            next_search_id: Arc::new(RwLock::new(1)),
            rss_items: Arc::new(RwLock::new(serde_json::Map::new())),
            rss_rules: Arc::new(RwLock::new(serde_json::Map::new())),
        }
    }

    pub fn with_registry(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        AppState {
            registry,
            engine: None,
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
            tracker_projection_cache: Arc::new(RwLock::new(HashMap::new())),
            preference_overrides: Arc::new(RwLock::new(serde_json::Map::new())),
            global_limits: Arc::new(RwLock::new(EngineGlobalLimits::default())),
            search_plugins: Arc::new(RwLock::new(serde_json::Map::new())),
            search_jobs: Arc::new(RwLock::new(serde_json::Map::new())),
            next_search_id: Arc::new(RwLock::new(1)),
            rss_items: Arc::new(RwLock::new(serde_json::Map::new())),
            rss_rules: Arc::new(RwLock::new(serde_json::Map::new())),
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        AppState {
            registry,
            engine: Some(engine),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
            tracker_projection_cache: Arc::new(RwLock::new(HashMap::new())),
            preference_overrides: Arc::new(RwLock::new(serde_json::Map::new())),
            global_limits: Arc::new(RwLock::new(EngineGlobalLimits::default())),
            search_plugins: Arc::new(RwLock::new(serde_json::Map::new())),
            search_jobs: Arc::new(RwLock::new(serde_json::Map::new())),
            next_search_id: Arc::new(RwLock::new(1)),
            rss_items: Arc::new(RwLock::new(serde_json::Map::new())),
            rss_rules: Arc::new(RwLock::new(serde_json::Map::new())),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
