use std::sync::Arc;

use sqlx::SqlitePool;

use crate::security::SecretBox;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub secrets: Arc<SecretBox>,
}

impl AppState {
    pub fn new(pool: SqlitePool, secrets: SecretBox) -> Self {
        Self {
            pool,
            secrets: Arc::new(secrets),
        }
    }
}
