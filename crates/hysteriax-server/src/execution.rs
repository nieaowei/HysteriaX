//! Database-time resource ownership and durable external-operation intent.
//!
//! A lease fences database writes, never an already dispatched SSH/DNS request.
//! Uncertain operations keep their resource blocked after expiry until verified.
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row};

pub const LEASE_SECONDS: i64 = 60;
pub const RENEW_SECONDS: u64 = 10;

#[derive(Clone, Debug)]
pub struct Execution {
    pub resource: String,
    pub owner: String,
    pub generation: i64,
    pub work: String,
}

/// Call inside the same transaction that claims the work item. For multiple
/// resources, call in resource-key order and roll back unless all are acquired.
pub async fn acquire(
    connection: &mut PgConnection,
    resource: &str,
    owner: &str,
    work: &str,
) -> Result<Option<Execution>, sqlx::Error> {
    sqlx::query("INSERT INTO resource_leases(resource_key) VALUES($1) ON CONFLICT DO NOTHING")
        .bind(resource)
        .execute(&mut *connection)
        .await?;
    let Some(row) = sqlx::query("SELECT owner,generation,blocked,(expires_at IS NULL OR expires_at<=clock_timestamp()) AS available FROM resource_leases WHERE resource_key=$1 FOR UPDATE SKIP LOCKED")
        .bind(resource).fetch_optional(&mut *connection).await? else { return Ok(None); };
    if row.get::<bool, _>("blocked") || !row.get::<bool, _>("available") {
        return Ok(None);
    }
    let uncertain: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_operations WHERE resource_key=$1 AND state IN ('dispatching','reconciling','needs_attention'))")
        .bind(resource).fetch_one(&mut *connection).await?;
    if uncertain {
        sqlx::query("UPDATE resource_leases SET blocked=true WHERE resource_key=$1")
            .bind(resource)
            .execute(&mut *connection)
            .await?;
        sqlx::query("UPDATE execution_operations SET state='reconciling',updated_at=clock_timestamp() WHERE resource_key=$1 AND state='dispatching'")
            .bind(resource).execute(&mut *connection).await?;
        return Ok(None);
    }
    let generation: i64 = sqlx::query_scalar("UPDATE resource_leases SET owner=$2,work_id=$3,generation=generation+1,expires_at=clock_timestamp()+make_interval(secs=>$4::double precision) WHERE resource_key=$1 RETURNING generation")
        .bind(resource).bind(owner).bind(work).bind(LEASE_SECONDS as f64)
        .fetch_one(&mut *connection).await?;
    Ok(Some(Execution {
        resource: resource.into(),
        owner: owner.into(),
        generation,
        work: work.into(),
    }))
}

impl Execution {
    /// Hold this row lock through the guarded progress/result transaction.
    pub async fn guard(&self, connection: &mut PgConnection) -> Result<(), sqlx::Error> {
        let valid: Option<bool> = sqlx::query_scalar("SELECT true FROM resource_leases WHERE resource_key=$1 AND owner=$2 AND generation=$3 AND work_id=$4 AND expires_at>clock_timestamp() AND NOT blocked FOR UPDATE")
            .bind(&self.resource).bind(&self.owner).bind(self.generation).bind(&self.work)
            .fetch_optional(connection).await?;
        if valid.is_none() {
            return Err(sqlx::Error::Protocol("execution lease was lost".into()));
        }
        Ok(())
    }

    pub async fn renew(&self, pool: &PgPool) -> Result<bool, sqlx::Error> {
        Ok(sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()+make_interval(secs=>$5::double precision) WHERE resource_key=$1 AND owner=$2 AND generation=$3 AND work_id=$4 AND expires_at>clock_timestamp() AND NOT blocked")
            .bind(&self.resource).bind(&self.owner).bind(self.generation).bind(&self.work).bind(LEASE_SECONDS as f64)
            .execute(pool).await?.rows_affected()==1)
    }

    pub async fn release(&self, connection: &mut PgConnection) -> Result<bool, sqlx::Error> {
        self.guard(connection).await?;
        Ok(sqlx::query("UPDATE resource_leases SET owner=NULL,expires_at=NULL,work_id=NULL WHERE resource_key=$1 AND NOT EXISTS(SELECT 1 FROM execution_operations WHERE resource_key=$1 AND state IN ('dispatching','reconciling','needs_attention'))")
            .bind(&self.resource).execute(connection).await?.rows_affected()==1)
    }

    /// Persist and COMMIT this intent before sending an external request. A
    /// returned operation ID already exists on retry; callers must reconcile it
    /// instead of sending the request again.
    pub async fn dispatch(
        &self,
        connection: &mut PgConnection,
        step: &str,
        intent: &Value,
    ) -> Result<(String, bool), sqlx::Error> {
        self.guard(connection).await?;
        let id = uuid::Uuid::new_v4().to_string();
        let inserted=sqlx::query("INSERT INTO execution_operations(operation_id,resource_key,owner,generation,work_id,step,state,intent) VALUES($1,$2,$3,$4,$5,$6,'dispatching',$7) ON CONFLICT(work_id,step) DO NOTHING")
            .bind(&id).bind(&self.resource).bind(&self.owner).bind(self.generation).bind(&self.work).bind(step).bind(intent)
            .execute(&mut *connection).await?.rows_affected()==1;
        let row = sqlx::query(
            "SELECT operation_id,intent FROM execution_operations WHERE work_id=$1 AND step=$2",
        )
        .bind(&self.work)
        .bind(step)
        .fetch_one(connection)
        .await?;
        if row.get::<Value, _>("intent") != *intent {
            return Err(sqlx::Error::Protocol(
                "operation intent changed; verification required".into(),
            ));
        }
        Ok((row.get("operation_id"), inserted))
    }

    /// Evidence must come from a provider receipt or explicit remote inspection.
    pub async fn verified(
        &self,
        connection: &mut PgConnection,
        operation: &str,
        evidence: &Value,
    ) -> Result<(), sqlx::Error> {
        self.guard(connection).await?;
        let changed=sqlx::query("UPDATE execution_operations SET state='verified',evidence=$5,updated_at=clock_timestamp() WHERE operation_id=$1 AND resource_key=$2 AND owner=$3 AND generation=$4 AND work_id=$6 AND state='dispatching'")
            .bind(operation).bind(&self.resource).bind(&self.owner).bind(self.generation).bind(evidence).bind(&self.work)
            .execute(connection).await?;
        if changed.rows_affected() != 1 {
            return Err(sqlx::Error::Protocol(
                "operation no longer belongs to this execution".into(),
            ));
        }
        Ok(())
    }
}

pub async fn heartbeat(pool: &PgPool, owner: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO service_instances(instance_id) VALUES($1) ON CONFLICT(instance_id) DO UPDATE SET heartbeat_at=clock_timestamp()")
        .bind(owner).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn claim(pool: &PgPool, resource: &str, owner: &str, work: &str) -> Option<Execution> {
        let mut tx = pool.begin().await.unwrap();
        let context = acquire(&mut tx, resource, owner, work).await.unwrap();
        tx.commit().await.unwrap();
        context
    }
    async fn expire(pool: &PgPool, resource: &str) {
        sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key=$1")
            .bind(resource).execute(pool).await.unwrap();
    }

    #[tokio::test]
    async fn expiry_fences_previous_owner_and_database_work_can_be_taken_over() {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        let a = claim(&pool, "node:a", "first", "work").await.unwrap();
        assert!(claim(&pool, "node:a", "second", "other").await.is_none());
        assert!(
            claim(&pool, "node:b", "second", "unrelated")
                .await
                .is_some()
        );
        expire(&pool, "node:a").await;
        assert!(!a.renew(&pool).await.unwrap());
        let b = claim(&pool, "node:a", "second", "work").await.unwrap();
        assert_eq!(b.generation, a.generation + 1);
        let mut tx = pool.begin().await.unwrap();
        assert!(a.guard(&mut tx).await.is_err());
        b.guard(&mut tx).await.unwrap();
        assert!(b.release(&mut tx).await.unwrap());
        tx.commit().await.unwrap();
        let c = claim(&pool, "node:a", "third", "next").await.unwrap();
        assert_eq!(c.generation, b.generation + 1);
    }

    #[tokio::test]
    async fn ambiguous_external_request_blocks_takeover_and_replay() {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        let a = claim(&pool, "dns:zone", "first", "change").await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let (operation, fresh) = a
            .dispatch(&mut tx, "create", &json!({"name":"example.test"}))
            .await
            .unwrap();
        assert!(fresh);
        tx.commit().await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let (same, fresh) = a
            .dispatch(&mut tx, "create", &json!({"name":"example.test"}))
            .await
            .unwrap();
        assert_eq!(same, operation);
        assert!(!fresh);
        assert!(!a.release(&mut tx).await.unwrap());
        tx.commit().await.unwrap();
        expire(&pool, "dns:zone").await;
        assert!(claim(&pool, "dns:zone", "second", "change").await.is_none());
        assert!(
            claim(&pool, "dns:zone", "third", "different-change")
                .await
                .is_none()
        );
        let (blocked,state): (bool,String)=sqlx::query_as("SELECT l.blocked,o.state FROM resource_leases l JOIN execution_operations o USING(resource_key) WHERE l.resource_key='dns:zone'")
            .fetch_one(&pool).await.unwrap();
        assert!(blocked);
        assert_eq!(state, "reconciling");
        let mut tx = pool.begin().await.unwrap();
        assert!(
            a.verified(&mut tx, &operation, &json!({"receipt":"late"}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn verified_operation_allows_release_and_live_owner_is_not_stolen() {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        heartbeat(&pool, "first").await.unwrap();
        heartbeat(&pool, "second").await.unwrap();
        let a = claim(&pool, "node:a", "first", "deploy").await.unwrap();
        assert!(a.renew(&pool).await.unwrap());
        assert!(claim(&pool, "node:a", "second", "deploy").await.is_none());
        let mut tx = pool.begin().await.unwrap();
        let (operation, _) = a
            .dispatch(&mut tx, "install", &json!({"digest":"abc"}))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        a.verified(&mut tx, &operation, &json!({"remote_digest":"abc"}))
            .await
            .unwrap();
        assert!(a.release(&mut tx).await.unwrap());
        tx.commit().await.unwrap();
        assert!(claim(&pool, "node:a", "second", "next").await.is_some());
    }
}

#[derive(Clone)]
struct Scope {
    pool: PgPool,
    leases: Vec<Execution>,
    sequence: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
tokio::task_local! { static CURRENT: Scope; }

/// Apply ownership checks inside every transaction opened by a leased worker.
/// The task-local scope never crosses into API handlers or unrelated workers.
pub async fn guard_current(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    let leases = CURRENT
        .try_with(|scope| scope.leases.clone())
        .unwrap_or_default();
    for lease in &leases {
        lease.guard(connection).await?;
    }
    if let Some(job) = leases
        .iter()
        .find(|lease| lease.resource.starts_with("job:"))
    {
        let running: Option<bool> =
            sqlx::query_scalar("SELECT true FROM jobs WHERE id=$1 AND status='running' FOR UPDATE")
                .bind(&job.work)
                .fetch_optional(&mut *connection)
                .await?;
        if running.is_none() {
            return Err(sqlx::Error::Protocol(
                "job was cancelled or no longer belongs to a running execution".into(),
            ));
        }
    }
    if let Some(work) = leases
        .iter()
        .find(|lease| lease.resource.starts_with("work:"))
    {
        let running:Option<bool>=sqlx::query_scalar("SELECT true FROM background_work w WHERE w.id=$1 AND w.owner=$2 AND w.generation=$3 AND w.state='running' AND (w.node_id IS NULL OR EXISTS(SELECT 1 FROM nodes n WHERE n.id=w.node_id AND n.state NOT IN ('deleting','delete_failed'))) FOR UPDATE OF w")
            .bind(&work.work).bind(&work.owner).bind(work.generation).fetch_optional(&mut *connection).await?;
        if running.is_none() {
            return Err(sqlx::Error::Protocol(
                "background execution was cancelled or ownership changed".into(),
            ));
        }
    }
    Ok(())
}

/// Renewal must not wait on its own guarded transaction's row locks. These
/// ordinary reads observe committed owner/generation/expiry and cancellation;
/// every result write and external dispatch still takes the stronger row locks.
async fn observe_current(scope: &Scope) -> Result<(), sqlx::Error> {
    for lease in &scope.leases {
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resource_leases WHERE resource_key=$1 AND owner=$2 AND generation=$3 AND work_id=$4 AND expires_at>clock_timestamp() AND NOT blocked)")
            .bind(&lease.resource).bind(&lease.owner).bind(lease.generation).bind(&lease.work).fetch_one(&scope.pool).await?;
        if !valid {
            return Err(sqlx::Error::Protocol("execution lease was lost".into()));
        }
    }
    if let Some(job) = scope.leases.iter().find(|l| l.resource.starts_with("job:")) {
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1 AND status='running')",
        )
        .bind(&job.work)
        .fetch_one(&scope.pool)
        .await?;
        if !active {
            return Err(sqlx::Error::Protocol("job execution was cancelled".into()));
        }
    }
    if let Some(work) = scope
        .leases
        .iter()
        .find(|l| l.resource.starts_with("work:"))
    {
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_work w WHERE w.id=$1 AND w.owner=$2 AND w.generation=$3 AND w.state='running' AND (w.node_id IS NULL OR EXISTS(SELECT 1 FROM nodes n WHERE n.id=w.node_id AND n.state NOT IN ('deleting','delete_failed'))))")
            .bind(&work.work).bind(&work.owner).bind(work.generation).fetch_one(&scope.pool).await?;
        if !active {
            return Err(sqlx::Error::Protocol(
                "background execution was cancelled or ownership changed".into(),
            ));
        }
    }
    Ok(())
}

/// Losing renewal cancels the local future. An external request may still run;
/// its dispatch intent remains durable and prevents releasing that resource.
pub async fn supervise<T>(
    pool: PgPool,
    leases: Vec<Execution>,
    work: impl std::future::Future<Output = T>,
) -> Option<T> {
    let scope = Scope {
        pool,
        leases,
        sequence: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };
    CURRENT.scope(scope.clone(), async move {
        // Validate before polling the work future at all; the renewal future's
        // first database await otherwise lets a stale worker start local work.
        if let Err(error)=check_current().await {
            tracing::warn!(%error,"execution ownership was lost before work started");
            return None;
        }
        let renew=async {
            loop {
                let mut contended=false;
                for lease in &scope.leases {
                    match lease.renew(&scope.pool).await {
                        Ok(true)=>{},
                        Err(error) if error.as_database_error().and_then(|e|e.code()).as_deref()==Some("55P03")=>{
                            // A guarded result transaction holds these same
                            // rows. A lock timeout is contention, not evidence
                            // of takeover; observe committed ownership below.
                            contended=true;
                        },
                        result=>{
                            tracing::warn!(resource=%lease.resource,owner=%lease.owner,generation=lease.generation,?result,"execution renewal lost; stopping local work");
                            return;
                        }
                    }
                }
                if let Err(error)=observe_current(&scope).await {
                    tracing::warn!(%error,"execution was cancelled or ownership expired; stopping local work");
                    return;
                }
                let delay=if contended {std::time::Duration::from_millis(250)}else{std::time::Duration::from_secs(RENEW_SECONDS)};
                tokio::time::sleep(delay).await;
            }
        };
        tokio::select! {
            biased;
            _=renew=>None,
            output=work=>Some(output),
        }
    }).await
}

pub async fn run_heartbeats(state: crate::state::AppState) {
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(RENEW_SECONDS));
    loop {
        timer.tick().await;
        if let Err(error) = heartbeat(&state.pool, &state.instance_id).await {
            tracing::warn!(%error,instance_id=%state.instance_id,"instance heartbeat failed");
        }
    }
}

/// Fence a read-only network request against the current resource ownership.
pub async fn check_current() -> anyhow::Result<()> {
    let scope = CURRENT.try_with(Clone::clone).ok();
    if let Some(scope) = scope {
        let mut tx = scope.pool.begin().await?;
        guard_current(&mut tx).await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Commit dispatch intent before polling a mutating network future. Persist
/// only a digest of request content: shell scripts and bodies can hold secrets.
/// A failed/aborted future deliberately leaves an uncertain operation behind.
pub async fn remote<T>(
    kind: &str,
    digest: String,
    request: impl std::future::Future<Output = anyhow::Result<T>>,
    evidence: impl FnOnce(&T) -> Value,
) -> anyhow::Result<T> {
    let Some(scope) = CURRENT.try_with(Clone::clone).ok() else {
        return request.await;
    };
    let prefix = if kind.starts_with("dns") {
        "dns-"
    } else {
        "node:"
    };
    let lease = scope
        .leases
        .iter()
        .find(|lease| lease.resource.starts_with(prefix))
        .or_else(|| scope.leases.first())
        .ok_or_else(|| anyhow::anyhow!("remote work requires a resource lease"))?;
    let index = scope
        .sequence
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let step = format!("{kind}:{index}");
    let mut tx = scope.pool.begin().await?;
    guard_current(&mut tx).await?;
    let (operation, fresh) = lease
        .dispatch(
            &mut tx,
            &step,
            &serde_json::json!({"kind":kind,"request_digest":digest}),
        )
        .await?;
    tx.commit().await?;
    anyhow::ensure!(
        fresh,
        "operation was previously dispatched; remote verification is required before recovery"
    );
    let output = request.await?;
    let mut tx = scope.pool.begin().await?;
    guard_current(&mut tx).await?;
    lease
        .verified(&mut tx, &operation, &evidence(&output))
        .await?;
    tx.commit().await?;
    Ok(output)
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    async fn fixture() -> (PgPool, Execution) {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        let mut tx = pool.begin().await.unwrap();
        let lease = acquire(&mut tx, "node:a", "instance-a", "work")
            .await
            .unwrap()
            .unwrap();
        tx.commit().await.unwrap();
        (pool, lease)
    }

    #[tokio::test]
    async fn a_guarded_database_batch_does_not_lose_ownership_to_its_own_renewal_lock_wait() {
        let (pool, lease) = fixture().await;
        sqlx::query("CREATE TABLE guarded_progress(value bigint)")
            .execute(&pool)
            .await
            .unwrap();
        let result = supervise(
            pool.clone(),
            vec![lease.clone()],
            crate::db::write::<_, anyhow::Error, _>(&pool, |tx| {
                Box::pin(async move {
                    sqlx::query("SELECT pg_sleep(13)")
                        .execute(&mut **tx)
                        .await?;
                    sqlx::query("INSERT INTO guarded_progress VALUES(42)")
                        .execute(&mut **tx)
                        .await?;
                    Ok(42)
                })
            }),
        )
        .await;
        assert_eq!(result.unwrap().unwrap(), 42);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM guarded_progress WHERE value=42")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert!(lease.renew(&pool).await.unwrap());
    }

    #[tokio::test]
    async fn renewal_contention_still_observes_committed_job_cancellation() {
        let (pool, node) = fixture().await;
        sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('work','sync','running','running',now(),now(),now())").execute(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let job = acquire(&mut tx, "job:work", "instance-a", "work")
            .await
            .unwrap()
            .unwrap();
        tx.commit().await.unwrap();
        let (guarded, ready) = tokio::sync::oneshot::channel();
        let other = pool.clone();
        let cancel = tokio::spawn(async move {
            ready.await.unwrap();
            sqlx::query("UPDATE jobs SET status='cancelled',stage='cancelled' WHERE id='work'")
                .execute(&other)
                .await
                .unwrap();
        });
        let writes = Arc::new(AtomicUsize::new(0));
        let counter = writes.clone();
        let started = tokio::time::Instant::now();
        let result = supervise(pool.clone(), vec![job, node.clone()], async {
            let mut held = pool.begin().await.unwrap();
            // Hold only the node lease to make renewal contend while the job's
            // persisted cancellation remains independently writable.
            node.guard(&mut held).await.unwrap();
            guarded.send(()).unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            counter.fetch_add(1, Ordering::SeqCst);
            held.commit().await.unwrap();
        })
        .await;
        cancel.await.unwrap();
        assert!(result.is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(15));
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn persisted_cancellation_prevents_new_dispatch_even_with_a_live_lease() {
        let (pool, node) = fixture().await;
        sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('work','sync','cancelled','cancelled',now(),now(),now())")
            .execute(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let job = acquire(&mut tx, "job:work", "instance-a", "work")
            .await
            .unwrap()
            .unwrap();
        tx.commit().await.unwrap();
        let sends = Arc::new(AtomicUsize::new(0));
        let counter = sends.clone();
        let result = supervise(
            pool.clone(),
            vec![job, node],
            remote(
                "ssh-command",
                "digest".into(),
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |_| serde_json::json!({"acknowledged":true}),
            ),
        )
        .await;
        assert!(result.is_none(), "cancelled work must not be polled at all");
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM execution_operations")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn lost_response_persists_intent_and_prevents_another_send() {
        let (pool, lease) = fixture().await;
        let sends = Arc::new(AtomicUsize::new(0));
        let counter = sends.clone();
        let outcome = supervise(
            pool.clone(),
            vec![lease.clone()],
            remote(
                "ssh-command",
                "request-digest".into(),
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(anyhow::anyhow!("response lost after remote execution"))
                },
                |_| serde_json::json!({"complete":true}),
            ),
        )
        .await;
        assert!(outcome.unwrap().is_err());
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        let state: String =
            sqlx::query_scalar("SELECT state FROM execution_operations WHERE work_id='work'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(state, "dispatching");
        sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(
            acquire(&mut tx, "node:a", "instance-b", "work")
                .await
                .unwrap()
                .is_none()
        );
        tx.commit().await.unwrap();
        let counter = sends.clone();
        let outcome = supervise(
            pool.clone(),
            vec![lease],
            remote(
                "ssh-command",
                "request-digest".into(),
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |_| serde_json::json!({"complete":true}),
            ),
        )
        .await;
        assert!(outcome.is_none());
        assert_eq!(sends.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn acknowledged_request_is_not_sent_again_when_work_is_reentered() {
        let (pool, lease) = fixture().await;
        let sends = Arc::new(AtomicUsize::new(0));
        for expected in [true, false] {
            let counter = sends.clone();
            let outcome = supervise(
                pool.clone(),
                vec![lease.clone()],
                remote(
                    "dns-request",
                    "request-digest".into(),
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                    |_| serde_json::json!({"provider_receipt":"stable"}),
                ),
            )
            .await;
            assert_eq!(outcome.unwrap().is_ok(), expected);
        }
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        let (state, evidence): (String, Value) =
            sqlx::query_as("SELECT state,evidence FROM execution_operations WHERE work_id='work'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(state, "verified");
        assert_eq!(evidence["provider_receipt"], "stable");
    }
}
