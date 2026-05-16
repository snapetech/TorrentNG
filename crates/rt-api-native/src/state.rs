use std::sync::Arc;
use tokio::sync::RwLock;

use rt_engine::EngineHandle;
use rt_session::SessionRegistry;

/// Shared application state threaded through axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            engine: None,
        }
    }

    pub fn with_registry(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        AppState {
            registry,
            engine: None,
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        AppState {
            registry,
            engine: Some(engine),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
