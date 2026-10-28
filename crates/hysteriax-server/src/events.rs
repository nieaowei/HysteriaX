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
mod tests {
    use super::*;
    use sqlx::Row;

    async fn read_database(pool: &PgPool, cursor: i64) -> Result<Vec<PersistedEvent>, sqlx::Error> {
        let rows=sqlx::query("SELECT p.publication_seq AS id,e.id AS source_id,e.event_type,e.payload_json FROM event_publications p JOIN job_events e ON e.id=p.event_id WHERE p.publication_seq>$1 ORDER BY p.publication_seq LIMIT 256")
        .bind(cursor).fetch_all(pool).await?;
        Ok(rows
            .into_iter()
            .map(|row| PersistedEvent {
                id: row.get("id"),
                source_id: row.get("source_id"),
                kind: row.get("event_type"),
                data: row.get::<Value, _>("payload_json").to_string(),
            })
            .collect())
    }

    async fn fixture(pool: &PgPool) {
        sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('a','sync','queued','queued',now(),now(),now()),('b','sync','queued','queued',now(),now(),now())")
            .execute(pool).await.unwrap();
    }

    #[tokio::test]
    async fn late_commit_with_smaller_source_id_is_published_after_the_head() {
        let pool = crate::db::test_pool_with_max_connections(4).await;
        fixture(&pool).await;
        let mut early = pool.begin().await.unwrap();
        let first: i64 = sqlx::query_scalar("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('a','early','{}',now()) RETURNING id")
            .fetch_one(&mut *early).await.unwrap();
        let second: i64 = sqlx::query_scalar("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('b','later','{}',now()) RETURNING id")
            .fetch_one(&pool).await.unwrap();
        assert!(first < second);
        assert_eq!(publish_pending(&pool).await.unwrap(), 1);
        let published = read_database(&pool, 0).await.unwrap();
        assert_eq!(published[0].source_id, second);
        let head = published[0].id;
        early.commit().await.unwrap();
        assert_eq!(publish_pending(&pool).await.unwrap(), 1);
        let resumed = read_database(&pool, head).await.unwrap();
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].source_id, first);
        assert_eq!(resumed[0].id, head + 1);
    }

    #[tokio::test]
    async fn concurrent_publishers_publish_once_and_rollback_does_not_advance_head() {
        let pool = crate::db::test_pool_with_max_connections(4).await;
        fixture(&pool).await;
        sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) SELECT 'a','event','{}',now() FROM generate_series(1,600)")
            .execute(&pool).await.unwrap();
        let mut rolled_back = pool.begin().await.unwrap();
        sqlx::query("UPDATE event_publication_stream SET head=999 WHERE id")
            .execute(&mut *rolled_back)
            .await
            .unwrap();
        rolled_back.rollback().await.unwrap();
        let (a, b, c) = tokio::join!(
            publish_pending(&pool),
            publish_pending(&pool),
            publish_pending(&pool)
        );
        let sizes = [a.unwrap(), b.unwrap(), c.unwrap()];
        assert!(sizes.iter().all(|size| *size <= 256));
        assert_eq!(sizes.iter().sum::<usize>(), 600);
        assert_eq!(publish_pending(&pool).await.unwrap(), 0);
        let (count,distinct,head): (i64,i64,i64) = sqlx::query_as("SELECT count(*),count(DISTINCT event_id),(SELECT head FROM event_publication_stream WHERE id) FROM event_publications")
            .fetch_one(&pool).await.unwrap();
        assert_eq!((count, distinct, head), (600, 600, 600));
    }

    #[tokio::test]
    async fn subscribers_observe_publication_and_epoch_changes() {
        let pool = crate::db::test_pool().await;
        fixture(&pool).await;
        let hub = Arc::new(EventHub::new());
        let mut first = hub.subscribe(pool.clone());
        let mut second = hub.subscribe(pool.clone());
        async fn updated(receiver: &mut watch::Receiver<u64>) {
            tokio::time::timeout(std::time::Duration::from_secs(5), receiver.changed())
                .await
                .unwrap()
                .unwrap();
        }
        updated(&mut first).await;
        updated(&mut second).await;
        assert_eq!(hub.stats()["watermark"], 0);
        sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('a','new','{}',now())")
            .execute(&pool).await.unwrap();
        updated(&mut first).await;
        updated(&mut second).await;
        assert_eq!(hub.stats()["watermark"], 1);
        let epoch = uuid::Uuid::new_v4().to_string();
        sqlx::query("UPDATE event_publication_stream SET epoch=$1 WHERE id")
            .bind(&epoch)
            .execute(&pool)
            .await
            .unwrap();
        updated(&mut first).await;
        updated(&mut second).await;
        assert_eq!(hub.stats()["epoch"], epoch);
        assert_eq!(hub.stats()["watermark"], 1);
    }
}
