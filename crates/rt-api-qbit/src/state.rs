use std::sync::Arc;
use tokio::sync::RwLock;

use rt_session::SessionRegistry;

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
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
