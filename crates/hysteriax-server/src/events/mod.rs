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
    pub fn subscribe(
        self: &Arc<Self>,
        pool: PgPool,
        mut shutdown: watch::Receiver<bool>,
    ) -> watch::Receiver<u64> {
        let receiver = self.changed.subscribe();
        if !self.started.swap(true, Ordering::SeqCst) {
            let weak = Arc::downgrade(self);
            tokio::spawn(async move {
                let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        _ = timer.tick() => {}
                    }
                    if *shutdown.borrow() {
                        break;
                    }
                    let Some(hub) = weak.upgrade() else {
                        break;
                    };
                    if hub.changed.receiver_count() == 0 {
                        continue;
                    }
                    match sqlx::query_as::<_, (String, i64)>(
                        "SELECT epoch,head FROM event_publication_stream WHERE id",
                    )
                    .fetch_one(&pool)
                    .await
                    {
                        Ok(position) => {
                            let mut previous = hub.position.lock().unwrap_or_else(|e| e.into_inner());
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
    pub fn notify_published(&self) {
        self.changed.send_modify(|version| *version += 1);
    }
    pub fn stats(&self) -> Value {
        let position = self.position.lock().unwrap_or_else(|e| e.into_inner());
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
    let mut connection = pool.acquire().await?;
    let pending: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM event_publication_pending)")
            .fetch_one(&mut *connection)
            .await?;
    if !pending {
        return Ok(0);
    }
    use sqlx::Acquire;
    let mut tx = connection.begin().await?;
    let (head, last_published): (i64, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "SELECT head,last_published_at FROM event_publication_stream WHERE id FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;
    let pending: Vec<i64> = sqlx::query_scalar(
        "SELECT event_id FROM event_publication_pending ORDER BY event_id LIMIT 256",
    )
    .fetch_all(&mut *tx)
    .await?;
    if !pending.is_empty() {
        // Use one timestamp for the ordered batch, after taking the stream lock.
        let published_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "WITH published AS (
                INSERT INTO event_publications(publication_seq,event_id,published_at)
                SELECT $1+p.position,p.event_id,greatest($3::timestamptz,statement_timestamp())
                FROM unnest($2::bigint[]) WITH ORDINALITY p(event_id,position)
                RETURNING published_at
             ) SELECT max(published_at) FROM published",
        )
        .bind(head)
        .bind(&pending)
        .bind(last_published)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM event_publication_pending WHERE event_id=ANY($1)")
            .bind(&pending)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE event_publication_stream SET head=$1,last_published_at=$2 WHERE id")
            .bind(head + pending.len() as i64)
            .bind(published_at)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(pending.len())
}

pub async fn run_publisher(state: crate::state::AppState) {
    loop {
        match publish_pending(&state.pool).await {
            Ok(count) => {
                if count > 0 {
                    state.events.notify_published();
                }
                if count == 256 {
                    continue;
                }
            }
            Err(error) => tracing::warn!(%error,"event outbox publisher failed"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests;
