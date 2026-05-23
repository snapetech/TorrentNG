use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::RwLock;

use rt_engine::EngineHandle;
use rt_session::SessionRegistry;

pub type JsonMap = BTreeMap<String, serde_json::Value>;

/// Shared application state threaded through axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub api_tokens: Arc<Vec<String>>,
    pub categories: Arc<RwLock<BTreeMap<String, String>>>,
    pub tags: Arc<RwLock<Vec<String>>>,
    pub saved_views: Arc<RwLock<JsonMap>>,
    pub ratio_groups: Arc<RwLock<JsonMap>>,
    pub workflows: Arc<RwLock<JsonMap>>,
    pub workflow_runs: Arc<RwLock<Vec<serde_json::Value>>>,
    pub rss_rules: Arc<RwLock<JsonMap>>,
    pub user_agent: Arc<RwLock<String>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            engine: None,
            api_tokens: Arc::new(Vec::new()),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(Vec::new())),
            saved_views: Arc::new(RwLock::new(BTreeMap::new())),
            ratio_groups: Arc::new(RwLock::new(BTreeMap::new())),
            workflows: Arc::new(RwLock::new(BTreeMap::new())),
            workflow_runs: Arc::new(RwLock::new(Vec::new())),
            rss_rules: Arc::new(RwLock::new(BTreeMap::new())),
            user_agent: Arc::new(RwLock::new(rt_engine::peer_id::user_agent().to_owned())),
        }
    }

    pub fn with_registry(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        AppState {
            registry,
            engine: None,
            api_tokens: Arc::new(Vec::new()),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(Vec::new())),
            saved_views: Arc::new(RwLock::new(BTreeMap::new())),
            ratio_groups: Arc::new(RwLock::new(BTreeMap::new())),
            workflows: Arc::new(RwLock::new(BTreeMap::new())),
            workflow_runs: Arc::new(RwLock::new(Vec::new())),
            rss_rules: Arc::new(RwLock::new(BTreeMap::new())),
            user_agent: Arc::new(RwLock::new(rt_engine::peer_id::user_agent().to_owned())),
        }
    }

    pub fn with_tokens(engine: Option<EngineHandle>, api_tokens: Vec<String>) -> Self {
        AppState {
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            engine,
            api_tokens: Arc::new(api_tokens),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(Vec::new())),
            saved_views: Arc::new(RwLock::new(BTreeMap::new())),
            ratio_groups: Arc::new(RwLock::new(BTreeMap::new())),
            workflows: Arc::new(RwLock::new(BTreeMap::new())),
            workflow_runs: Arc::new(RwLock::new(Vec::new())),
            rss_rules: Arc::new(RwLock::new(BTreeMap::new())),
            user_agent: Arc::new(RwLock::new(rt_engine::peer_id::user_agent().to_owned())),
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        AppState {
            registry,
            engine: Some(engine),
            api_tokens: Arc::new(Vec::new()),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(Vec::new())),
            saved_views: Arc::new(RwLock::new(BTreeMap::new())),
            ratio_groups: Arc::new(RwLock::new(BTreeMap::new())),
            workflows: Arc::new(RwLock::new(BTreeMap::new())),
            workflow_runs: Arc::new(RwLock::new(Vec::new())),
            rss_rules: Arc::new(RwLock::new(BTreeMap::new())),
            user_agent: Arc::new(RwLock::new(rt_engine::peer_id::user_agent().to_owned())),
        }
    }

    pub fn with_engine_and_tokens(
        registry: Arc<RwLock<SessionRegistry>>,
        engine: EngineHandle,
        api_tokens: Vec<String>,
    ) -> Self {
        AppState {
            registry,
            engine: Some(engine),
            api_tokens: Arc::new(api_tokens),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(Vec::new())),
            saved_views: Arc::new(RwLock::new(BTreeMap::new())),
            ratio_groups: Arc::new(RwLock::new(BTreeMap::new())),
            workflows: Arc::new(RwLock::new(BTreeMap::new())),
            workflow_runs: Arc::new(RwLock::new(Vec::new())),
            rss_rules: Arc::new(RwLock::new(BTreeMap::new())),
            user_agent: Arc::new(RwLock::new(rt_engine::peer_id::user_agent().to_owned())),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
