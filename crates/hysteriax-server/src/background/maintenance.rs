//! Durable scheduling for bounded, database-only maintenance batches.
//! Remote and node-owned work must use their additional resource leases.
use crate::{
    db,
    execution::{self, Execution},
};
use anyhow::Result;
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};

#[derive(Clone, Debug)]
pub struct Work {
    pub id: String,
    pub payload: Value,
    pub leases: Vec<Execution>,
}

/// A recurring key has one durable work row and one monotonically increasing
/// work lease. A live running owner is never reset by another scheduler.
pub async fn ensure(pool: &PgPool, key: &str, kind: &str, payload: Value) -> Result<()> {
    let (id, kind) = (format!("maintenance:{key}"), kind.to_owned());
    db::write(pool,move |tx| {
        let (id,kind,payload)=(id.clone(),kind.clone(),payload.clone());
        Box::pin(async move {
            sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',finished_at=NULL,error_code=NULL WHERE background_work.state IN ('complete','failed') AND background_work.available_at<=clock_timestamp() AND background_work.kind=excluded.kind")
                .bind(id).bind(kind).bind(payload).execute(&mut **tx).await?;
            Ok(())
        })
    }).await
}

pub async fn claim(pool: &PgPool, kind: &str, owner: &str) -> Result<Option<Work>> {
    let (kind, owner) = (kind.to_owned(), owner.to_owned());
    db::write(pool,move |tx| {
        let (kind,owner)=(kind.clone(),owner.clone());
        Box::pin(async move {
            let now:chrono::DateTime<chrono::Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
            let rows=sqlx::query("SELECT id,payload FROM background_work w WHERE kind=$1 AND cycle_id IS NULL AND node_id IS NULL AND state IN ('queued','running') AND available_at<=$2 AND NOT EXISTS(SELECT 1 FROM resource_leases l WHERE l.resource_key='work:'||w.id AND (l.blocked OR (l.owner IS NOT NULL AND l.expires_at>clock_timestamp()) OR EXISTS(SELECT 1 FROM execution_operations o WHERE o.resource_key=l.resource_key AND o.state IN ('dispatching','reconciling','needs_attention')))) ORDER BY available_at,id LIMIT 32 FOR UPDATE OF w SKIP LOCKED")
                .bind(&kind).bind(now).fetch_all(&mut **tx).await?;
            for row in rows {
                let id:String=row.get("id");
                let Some(lease)=execution::acquire(tx,&format!("work:{id}"),&owner,&id).await? else {continue;};
                sqlx::query("UPDATE background_work SET state='running',owner=$2,generation=$3,started_at=clock_timestamp(),finished_at=NULL WHERE id=$1")
                    .bind(&id).bind(&owner).bind(lease.generation).execute(&mut **tx).await?;
                return Ok(Some(Work{id,payload:row.get("payload"),leases:vec![lease]}));
            }
            Ok(None)
        })
    }).await
}

impl Work {
    pub async fn guard_on(&self, tx: &mut Transaction<'static, Postgres>) -> Result<()> {
        for lease in &self.leases {
            lease.guard(tx).await?;
        }
        let lease = &self.leases[0];
        let own:Option<bool>=sqlx::query_scalar("SELECT true FROM background_work WHERE id=$1 AND owner=$2 AND generation=$3 AND state='running' FOR UPDATE")
            .bind(&self.id).bind(&lease.owner).bind(lease.generation).fetch_optional(&mut **tx).await?;
        anyhow::ensure!(own == Some(true), "maintenance ownership was lost");
        Ok(())
    }

    /// Call in the same transaction as the batch's changes. Incomplete work
    /// yields its lease and resumes after a short delay; complete work schedules
    /// its next run using database time, independently of any instance's clock.
    pub async fn finish_on(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        more: bool,
        next_seconds: i64,
        error_code: Option<&str>,
    ) -> Result<()> {
        self.guard_on(tx).await?;
        let lease = &self.leases[0];
        let changed=sqlx::query("UPDATE background_work SET state=$4,owner=NULL,finished_at=CASE WHEN $4='complete' THEN clock_timestamp() ELSE NULL END,available_at=clock_timestamp()+make_interval(secs=>$5::double precision),error_code=$6 WHERE id=$1 AND owner=$2 AND generation=$3 AND state='running'")
            .bind(&self.id).bind(&lease.owner).bind(lease.generation).bind(if more{"queued"}else{"complete"}).bind(next_seconds as f64).bind(error_code).execute(&mut **tx).await?.rows_affected();
        anyhow::ensure!(changed == 1, "maintenance ownership was lost");
        for lease in &self.leases {
            lease.release(tx).await?;
        }
        Ok(())
    }
    pub async fn retry(&self, pool: &PgPool) -> Result<()> {
        let work = self.clone();
        db::write(pool, move |tx| {
            let work = work.clone();
            Box::pin(async move {
                work.finish_on(tx, true, 5, Some("maintenance_deferred"))
                    .await
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    async fn count(pool: &PgPool, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }
    async fn due(pool: &PgPool) {
        sqlx::query(
            "UPDATE background_work SET available_at=clock_timestamp()-interval '1 second'",
        )
        .execute(pool)
        .await
        .unwrap();
    }
    async fn finish(pool: &PgPool, work: &Work, more: bool) -> Result<()> {
        let work = work.clone();
        db::write(pool, move |tx| {
            let work = work.clone();
            Box::pin(async move {
                work.finish_on(tx, more, if more { 1 } else { 86400 }, None)
                    .await
            })
        })
        .await
    }
    #[tokio::test]
    async fn shared_maintenance_is_claimed_once_and_healthy_owners_are_not_reset() {
        let pool = db::test_pool_with_max_connections(4).await;
        let (a, b) = tokio::join!(
            ensure(&pool, "a", "test-maintenance", json!({"cursor":1})),
            ensure(&pool, "a", "test-maintenance", json!({"cursor":1}))
        );
        a.unwrap();
        b.unwrap();
        let (a, b) = tokio::join!(
            claim(&pool, "test-maintenance", "one"),
            claim(&pool, "test-maintenance", "two")
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(a.is_some(), b.is_some());
        let work = a.or(b).unwrap();
        ensure(&pool, "a", "test-maintenance", json!({"cursor":2}))
            .await
            .unwrap();
        assert_eq!(count(&pool,"SELECT count(*) FROM background_work WHERE state='running' AND payload->>'cursor'='1'").await,1);
        assert_eq!(
            crate::background::expire(&pool).await.unwrap(),
            0,
            "sampling expiry must not expire nodeless maintenance"
        );
        assert!(
            claim(&pool, "test-maintenance", "three")
                .await
                .unwrap()
                .is_none()
        );
        ensure(&pool, "b", "test-maintenance", json!({}))
            .await
            .unwrap();
        let other = claim(&pool, "test-maintenance", "three")
            .await
            .unwrap()
            .unwrap();
        assert_ne!(work.id, other.id);
        let outcome=execution::supervise(pool.clone(),work.leases.clone(),db::write::<_,anyhow::Error,_>(&pool,|tx|Box::pin(async move {
            sqlx::query("UPDATE background_work SET error_code='guarded-write' WHERE id='maintenance:a'").execute(&mut **tx).await?;Ok(())
        }))).await;
        outcome.unwrap().unwrap();
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM background_work WHERE error_code='guarded-write'"
            )
            .await,
            1
        );
    }
    #[tokio::test]
    async fn maintenance_takeover_fences_old_progress_and_completion_and_reuses_the_key() {
        let pool = db::test_pool_with_max_connections(3).await;
        ensure(&pool, "a", "test-maintenance", json!({"cursor":7}))
            .await
            .unwrap();
        let old = claim(&pool, "test-maintenance", "old")
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        let new = claim(&pool, "test-maintenance", "new")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(new.id, old.id);
        assert_eq!(new.payload, old.payload);
        assert!(new.leases[0].generation > old.leases[0].generation);
        assert!(finish(&pool, &old, false).await.is_err());
        let rejected = execution::supervise(pool.clone(), old.leases.clone(), async {
            panic!("stale work was polled")
        })
        .await;
        assert!(rejected.is_none());
        new.retry(&pool).await.unwrap();
        assert_eq!(count(&pool,"SELECT count(*) FROM background_work WHERE state='queued' AND error_code='maintenance_deferred'").await,1);
        assert!(
            claim(&pool, "test-maintenance", "next")
                .await
                .unwrap()
                .is_none()
        );
        due(&pool).await;
        let next = claim(&pool, "test-maintenance", "next")
            .await
            .unwrap()
            .unwrap();
        finish(&pool, &next, false).await.unwrap();
        ensure(&pool, "a", "test-maintenance", json!({}))
            .await
            .unwrap();
        assert!(
            claim(&pool, "test-maintenance", "early")
                .await
                .unwrap()
                .is_none()
        );
        due(&pool).await;
        ensure(&pool, "a", "test-maintenance", json!({}))
            .await
            .unwrap();
        let recurring = claim(&pool, "test-maintenance", "recurring")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recurring.id, old.id);
        assert!(recurring.leases[0].generation > next.leases[0].generation);
        assert_eq!(recurring.payload, json!({"cursor":7}));
    }
    #[tokio::test]
    async fn claimed_maintenance_rows_are_skipped_and_progress_acknowledgement_is_atomic() {
        let pool = db::test_pool_with_max_connections(3).await;
        ensure(&pool, "a", "test-maintenance", json!({}))
            .await
            .unwrap();
        ensure(&pool, "b", "test-maintenance", json!({}))
            .await
            .unwrap();
        let mut held = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM background_work WHERE id='maintenance:a' FOR UPDATE")
            .fetch_one(&mut *held)
            .await
            .unwrap();
        let work = claim(&pool, "test-maintenance", "owner")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(work.id, "maintenance:b");
        held.rollback().await.unwrap();
        let aborted: Result<()> = db::write(&pool, |tx| {
            let work = work.clone();
            Box::pin(async move {
                work.guard_on(tx).await?;
                sqlx::query(
                    "UPDATE background_work SET payload='{\"cursor\":99}' WHERE id='maintenance:b'",
                )
                .execute(&mut **tx)
                .await?;
                work.finish_on(tx, false, 86400, None).await?;
                anyhow::bail!("rollback all progress and acknowledgement")
            })
        })
        .await;
        assert!(aborted.is_err());
        assert_eq!(count(&pool,"SELECT count(*) FROM background_work WHERE id='maintenance:b' AND state='running' AND payload='{}'").await,1);
        assert!(work.leases[0].renew(&pool).await.unwrap());
        finish(&pool, &work, false).await.unwrap();
    }
}
