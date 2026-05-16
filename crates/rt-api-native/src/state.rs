use std::sync::Arc;
use tokio::sync::RwLock;

use rt_session::SessionRegistry;

/// Shared application state threaded through axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
        }
    }

    pub fn with_registry(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        AppState { registry }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
