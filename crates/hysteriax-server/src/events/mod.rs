//! Lightweight publication notifications; v2 streams read durable events directly.
use serde_json::Value;
use sqlx::PgPool;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;

pub struct PersistedEvent {
    pub id: i64,
    pub source_id: i64,
    pub kind: String,
    pub data: String,
}
impl PersistedEvent {
    /// The stable source ID supports de-duplication independently of resume order.
    pub fn wire_data(&self) -> String {
        let mut payload: Value = serde_json::from_str(&self.data).unwrap_or(Value::Null);
        if let Some(object) = payload.as_object_mut() {
            object.insert("event_id".into(), self.source_id.into());
            payload.to_string()
        } else {
            self.data.clone()
        }
    }
}

pub struct EventHub {
    position: Mutex<Option<(String, i64)>>,
    started: AtomicBool,
    changed: watch::Sender<u64>,
}
impl EventHub {
    pub fn new() -> Self {
        Self {
            position: Mutex::new(None),
            started: AtomicBool::new(false),
            changed: watch::channel(0).0,
        }
    }
    pub fn subscribe(self: &Arc<Self>, pool: PgPool) -> watch::Receiver<u64> {
        let receiver = self.changed.subscribe();
        if !self.started.swap(true, Ordering::SeqCst) {
            let weak = Arc::downgrade(self);
            tokio::spawn(async move {
                let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    timer.tick().await;
                    let Some(hub) = weak.upgrade() else {
                        break;
                    };
                    if let Err(error) = publish_pending(&pool).await {
                        tracing::warn!(%error, "event publication failed");
                        continue;
                    }
                    match sqlx::query_as::<_, (String, i64)>(
                        "SELECT epoch,head FROM event_publication_stream WHERE id",
                    )
                    .fetch_one(&pool)
                    .await
                    {
                        Ok(position) => {
                            let mut previous = hub.position.lock().unwrap();
                            if previous.as_ref() != Some(&position) {
                                *previous = Some(position);
                                drop(previous);
                                hub.changed.send_modify(|version| *version += 1);
                            }
                        }
                        Err(error) => tracing::warn!(%error, "event notification poll failed"),
                    }
                }
            });
        }
        receiver
    }
    pub fn stats(&self) -> Value {
        let position = self.position.lock().unwrap();
        serde_json::json!({
            "epoch": position.as_ref().map(|(epoch, _)| epoch),
            "watermark": position.as_ref().map(|(_, head)| *head),
            "running": self.started.load(Ordering::Relaxed),
        })
    }
}

/// Publish a bounded batch of committed outbox rows. The stream row serializes
/// publishers through commit, so a resume cursor never skips a late commit.
/// No business transaction acquires this lock.
pub async fn publish_pending(pool: &PgPool) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut head: i64 =
        sqlx::query_scalar("SELECT head FROM event_publication_stream WHERE id FOR UPDATE")
            .fetch_one(&mut *tx)
            .await?;
    let pending: Vec<i64> = sqlx::query_scalar(
        "SELECT event_id FROM event_publication_pending ORDER BY event_id LIMIT 256",
    )
    .fetch_all(&mut *tx)
    .await?;
    for id in &pending {
        head += 1;
        sqlx::query("INSERT INTO event_publications(publication_seq,event_id) VALUES($1,$2)")
            .bind(head)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if !pending.is_empty() {
        sqlx::query("DELETE FROM event_publication_pending WHERE event_id=ANY($1)")
            .bind(&pending)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE event_publication_stream SET head=$1 WHERE id")
            .bind(head)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(pending.len())
}

pub async fn run_publisher(state: crate::state::AppState) {
    loop {
        match publish_pending(&state.pool).await {
            Ok(256) => continue,
            Ok(_) => {}
            Err(error) => tracing::warn!(%error,"event outbox publisher failed"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests;
