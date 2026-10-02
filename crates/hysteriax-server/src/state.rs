use std::sync::Arc;

use sqlx::PgPool;

use crate::security::SecretBox;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub secrets: Arc<SecretBox>,
    pub monitor: Arc<crate::monitoring::Monitor>,
}

impl AppState {
    pub fn new(pool: PgPool, secrets: SecretBox) -> Self {
        Self {
            pool,
            secrets: Arc::new(secrets),
            monitor: Arc::new(crate::monitoring::Monitor::new()),
        }
    }
}
