//! One durable event reader per service; bounded fan-out with database replay for old cursors.
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;

pub struct PersistedEvent {
    pub id: i64,
    pub job_id: String,
    pub kind: String,
    pub data: String,
}
struct Buffer {
    events: VecDeque<Arc<PersistedEvent>>,
    bytes: usize,
    watermark: i64,
    ready: bool,
}
pub enum Read {
    Live(Vec<Arc<PersistedEvent>>, i64),
    Replay(i64),
    Waiting,
}
pub struct EventHub {
    buffer: Mutex<Buffer>,
    started: AtomicBool,
    changed: watch::Sender<u64>,
}
impl EventHub {
    pub fn new() -> Self {
        Self {
            buffer: Mutex::new(Buffer {
                events: VecDeque::new(),
                bytes: 0,
                watermark: 0,
                ready: false,
            }),
            started: AtomicBool::new(false),
            changed: watch::channel(0).0,
        }
    }
    pub fn subscribe(self: &Arc<Self>, pool: PgPool) -> watch::Receiver<u64> {
        let receiver = self.changed.subscribe();
        if !self.started.swap(true, Ordering::SeqCst) {
            let weak = Arc::downgrade(self);
            tokio::spawn(async move {
                let mut cursor = None;
                let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    timer.tick().await;
                    let Some(hub) = weak.upgrade() else {
                        break;
                    };
                    if cursor.is_none() {
                        match sqlx::query_scalar::<_, i64>(
                            "SELECT coalesce(max(id),0) FROM job_events",
                        )
                        .fetch_one(&pool)
                        .await
                        {
                            Ok(id) => {
                                cursor = Some(id);
                                let mut buffer = hub.buffer.lock().unwrap();
                                buffer.watermark = id;
                                buffer.ready = true;
                                drop(buffer);
                                hub.changed.send_modify(|version| *version += 1);
                            }
                            Err(error) => {
                                tracing::warn!(%error,"event reader initialization failed");
                                continue;
                            }
                        }
                    }
                    loop {
                        match read_database(&pool, cursor.unwrap(), i64::MAX, None).await {
                            Ok(events) => {
                                let full = events.len() == 256;
                                if !events.is_empty() {
                                    let mut buffer = hub.buffer.lock().unwrap();
                                    for event in events {
                                        cursor = Some(event.id);
                                        buffer.watermark = event.id;
                                        buffer.bytes += event.data.len();
                                        buffer.events.push_back(event);
                                    }
                                    while buffer.events.len() > 4096
                                        || buffer.bytes > 4 * 1024 * 1024
                                    {
                                        if let Some(event) = buffer.events.pop_front() {
                                            buffer.bytes -= event.data.len();
                                        }
                                    }
                                    drop(buffer);
                                    hub.changed.send_modify(|version| *version += 1);
                                }
                                if !full {
                                    break;
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error,"shared event reader failed");
                                break;
                            }
                        }
                    }
                }
            });
        }
        receiver
    }
    pub fn read(&self, cursor: i64, job_id: Option<&str>) -> Read {
        let buffer = self.buffer.lock().unwrap();
        if !buffer.ready {
            return Read::Waiting;
        }
        if cursor < buffer.watermark
            && buffer
                .events
                .front()
                .is_none_or(|event| cursor < event.id - 1)
        {
            return Read::Replay(buffer.watermark);
        }
        Read::Live(
            buffer
                .events
                .iter()
                .filter(|event| event.id > cursor && job_id.is_none_or(|job| job == event.job_id))
                .cloned()
                .collect(),
            buffer.watermark,
        )
    }
    pub fn stats(&self) -> Value {
        let buffer = self.buffer.lock().unwrap();
        serde_json::json!({"events":buffer.events.len(),"bytes":buffer.bytes,"watermark":buffer.watermark,"running":self.started.load(Ordering::Relaxed)})
    }
}
pub async fn read_database(
    pool: &PgPool,
    cursor: i64,
    until: i64,
    job_id: Option<&str>,
) -> Result<Vec<Arc<PersistedEvent>>, sqlx::Error> {
    let rows=sqlx::query("SELECT id,job_id,event_type,payload_json FROM job_events WHERE id>$1 AND id<=$2 AND ($3::text IS NULL OR job_id=$3) ORDER BY id LIMIT 256")
        .bind(cursor).bind(until).bind(job_id).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            Arc::new(PersistedEvent {
                id: row.get("id"),
                job_id: row.get("job_id"),
                kind: row.get("event_type"),
                data: row.get::<Value, _>("payload_json").to_string(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shared_reader_replays_history_filters_jobs_and_delivers_new_events() {
        let pool = crate::db::test_pool().await;
        for id in ["a", "b"] {
            sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES($1,'sync','queued','queued',now(),now(),now())").bind(id).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('a','old','{}',now()),('b','other','{}',now()),('a','old','{}',now())").execute(&pool).await.unwrap();
        let hub = Arc::new(EventHub::new());
        let mut updates = hub.subscribe(pool.clone());
        let _second = hub.subscribe(pool.clone());
        tokio::time::timeout(std::time::Duration::from_secs(3), updates.changed())
            .await
            .unwrap()
            .unwrap();
        let until = match hub.read(0, Some("a")) {
            Read::Replay(until) => until,
            _ => panic!("old cursor must use durable replay"),
        };
        let old = read_database(&pool, 0, until, Some("a")).await.unwrap();
        assert_eq!(old.len(), 2);
        let id:i64=sqlx::query_scalar("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('a','new','{}',now()) RETURNING id").fetch_one(&pool).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), updates.changed())
            .await
            .unwrap()
            .unwrap();
        match hub.read(until, Some("a")) {
            Read::Live(events, watermark) => {
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].id, id);
                assert_eq!(watermark, id);
            }
            _ => panic!("new events should be shared"),
        }
        match hub.read(until, Some("b")) {
            Read::Live(events, _) => assert!(events.is_empty()),
            _ => panic!("job filter must use shared events"),
        }
    }
}
