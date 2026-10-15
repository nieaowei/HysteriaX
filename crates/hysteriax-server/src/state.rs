use std::sync::Arc;

use sqlx::PgPool;

use crate::security::SecretBox;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub caches: Arc<crate::cache::RuntimeCaches>,
    pub quota_rank: Arc<crate::quota_rank::QuotaRank>,
    pub events: Arc<crate::events::EventHub>,
    pub secrets: Arc<SecretBox>,
    pub monitor: Arc<crate::monitoring::Monitor>,
    pub removed_nodes: tokio::sync::broadcast::Sender<String>,
}

impl AppState {
    pub fn new(pool: PgPool, secrets: SecretBox) -> Self {
        let secrets = Arc::new(secrets);
        let caches = Arc::new(crate::cache::RuntimeCaches::new(secrets.clone()));
        Self {
            pool,
            quota_rank: Arc::new(crate::quota_rank::QuotaRank::new(caches.redis.clone())),
            caches,
            events: Arc::new(crate::events::EventHub::new()),
            secrets,
            monitor: Arc::new(crate::monitoring::Monitor::new()),
            removed_nodes: tokio::sync::broadcast::channel(256).0,
        }
    }
}
