//! Encrypted shared cache; bounded I/O and a short circuit breaker keep reads available.
use crate::security::SecretBox;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use serde::{Serialize, de::DeserializeOwned};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;

pub struct RedisCache {
    client: redis::Client,
    connection: OnceCell<ConnectionManager>,
    secrets: Arc<SecretBox>,
    prefix: String,
    retry_after: Mutex<Option<Instant>>,
    errors: AtomicU64,
}
impl RedisCache {
    pub fn new(url: &str, prefix: String, secrets: Arc<SecretBox>) -> redis::RedisResult<Self> {
        Ok(Self {
            client: redis::Client::open(url)?,
            connection: OnceCell::new(),
            secrets,
            prefix,
            retry_after: Mutex::new(None),
            errors: AtomicU64::new(0),
        })
    }
    pub fn available(&self) -> bool {
        self.retry_after
            .lock()
            .unwrap()
            .is_none_or(|until| until <= Instant::now())
    }
    fn failed(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        *self.retry_after.lock().unwrap() = Some(Instant::now() + Duration::from_secs(2));
    }
    pub fn key(&self, kind: &str, logical: &str) -> String {
        format!(
            "{}:{}:{}",
            self.prefix,
            kind,
            crate::security::token_digest(logical)
        )
    }
    pub(crate) async fn command<T: redis::FromRedisValue>(&self, cmd: redis::Cmd) -> Option<T> {
        if !self.available() {
            return None;
        }
        let result = tokio::time::timeout(Duration::from_millis(150), async {
            let connection = self
                .connection
                .get_or_try_init(|| async {
                    ConnectionManager::new_with_config(
                        self.client.clone(),
                        ConnectionManagerConfig::new()
                            .set_number_of_retries(0)
                            .set_connection_timeout(Some(Duration::from_millis(100)))
                            .set_response_timeout(Some(Duration::from_millis(100))),
                    )
                    .await
                })
                .await?;
            cmd.query_async::<T>(&mut connection.clone()).await
        })
        .await;
        match result {
            Ok(Ok(value)) => Some(value),
            _ => {
                self.failed();
                None
            }
        }
    }
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let cipher: Option<String> = self.command(redis::cmd("GET").arg(key).to_owned()).await?;
        let cipher = cipher?;
        let value = self
            .secrets
            .decrypt(&cipher)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        if value.is_none() {
            self.delete(key).await;
        }
        value
    }
    pub async fn put<T: Serialize>(&self, key: &str, value: &T, ttl: Duration) {
        let Some(cipher) = serde_json::to_string(value)
            .ok()
            .and_then(|text| self.secrets.encrypt(&text).ok())
        else {
            return;
        };
        if cipher.len() > 8 * 1024 * 1024 || ttl.is_zero() {
            return;
        }
        let _: Option<()> = self
            .command(
                redis::cmd("SET")
                    .arg(key)
                    .arg(cipher)
                    .arg("PX")
                    .arg(ttl.as_millis().max(1) as u64)
                    .to_owned(),
            )
            .await;
    }
    pub async fn delete(&self, key: &str) {
        let _: Option<u64> = self.command(redis::cmd("DEL").arg(key).to_owned()).await;
    }
    pub fn stats(&self) -> serde_json::Value {
        serde_json::json!({"configured":true,"available":self.available(),"errors":self.errors.load(Ordering::Relaxed),"encrypted":true})
    }
}

#[cfg(test)]
mod tests;
