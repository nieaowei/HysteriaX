//! A completion intent belongs to one database attempt, never to an uncommitted
//! work row. Publish Committing before sending COMMIT, then resolve it before
//! polling any caller continuation. The renewal branch cannot discard that
//! continuation merely because this transaction released its own leases.
use super::{CURRENT, Execution};
use sqlx::{PgConnection, Postgres, Transaction};
use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    Released,
    Complete,
    Yielded,
    Failed,
    Quarantined,
}

#[derive(Clone, Debug)]
pub(super) enum Phase {
    Active,
    Committing {
        attempt: u64,
        disposition: Disposition,
    },
    Committed {
        attempt: u64,
        disposition: Disposition,
        released: Vec<Execution>,
    },
    RecoveryRequired(String),
    Lost(String),
}

pub(super) struct Lifecycle {
    pub leases: Vec<Execution>,
    phase: watch::Sender<Phase>,
    sequence: AtomicU64,
}
impl Lifecycle {
    pub fn new(leases: Vec<Execution>) -> Arc<Self> {
        Arc::new(Self {
            leases,
            phase: watch::channel(Phase::Active).0,
            sequence: AtomicU64::new(1),
        })
    }
    pub fn phase(&self) -> Phase {
        self.phase.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Phase> {
        self.phase.subscribe()
    }
    fn record(&self, attempt: u64, disposition: Disposition, state: &'static str) {
        if let Some(primary) = self
            .leases
            .iter()
            .find(|l| l.resource.starts_with("work:") || l.resource.starts_with("job:"))
            .or(self.leases.first())
        {
            tracing::debug!(work_id=%primary.work,resource=%primary.resource,owner=%primary.owner,
                generation=primary.generation,epoch=%primary.epoch,attempt,?disposition,state,
                "execution finalization transition");
        }
    }
    pub fn active(&self) -> Result<(), sqlx::Error> {
        match self.phase() {
            Phase::Active => Ok(()),
            _ => Err(sqlx::Error::Protocol(
                "execution has entered finalization; further work is forbidden".into(),
            )),
        }
    }
    pub fn lose(&self, reason: String) {
        self.phase.send_if_modified(|phase| {
            if matches!(phase, Phase::Active) {
                *phase = Phase::Lost(reason.clone());
                true
            } else {
                false
            }
        });
    }
    pub fn require_recovery(&self, reason: String) {
        self.phase.send_if_modified(|phase| {
            if matches!(phase, Phase::Committing { .. } | Phase::Committed { .. }) {
                *phase = Phase::RecoveryRequired(reason.clone());
                true
            } else {
                false
            }
        });
    }
    fn resolve(&self, attempt: u64, next: Phase) {
        self.phase.send_if_modified(|phase| {
            if matches!(phase, Phase::Committing { attempt: current, .. } if *current==attempt) {
                *phase = next.clone();
                true
            } else {
                false
            }
        });
    }
}

#[derive(Default)]
struct Intent {
    disposition: Option<Disposition>,
    released: Vec<Execution>,
}

pub(crate) struct Attempt {
    lifecycle: Option<Arc<Lifecycle>>,
    leases: Vec<Execution>,
    connection: usize,
    intent: Mutex<Intent>,
    id: u64,
    committing: bool,
}
tokio::task_local! { static ATTEMPT: Arc<AttemptContext>; }
struct AttemptContext {
    connection: usize,
    lifecycle: Option<Arc<Lifecycle>>,
    intent: Mutex<Intent>,
}

impl Attempt {
    pub fn new(connection: &mut PgConnection) -> Self {
        let lifecycle = CURRENT.try_with(|s| s.lifecycle.clone()).ok().flatten();
        let leases = CURRENT.try_with(|s| s.leases.clone()).unwrap_or_default();
        let id = lifecycle
            .as_ref()
            .map(|s| s.sequence.fetch_add(1, Ordering::Relaxed))
            .unwrap_or(0);
        Self {
            lifecycle,
            leases,
            connection: connection as *mut PgConnection as usize,
            intent: Mutex::new(Intent::default()),
            id,
            committing: false,
        }
    }
    pub async fn operation<T>(&self, work: impl Future<Output = T>) -> T {
        if self.lifecycle.is_none() {
            return work.await;
        }
        let context = Arc::new(AttemptContext {
            connection: self.connection,
            lifecycle: self.lifecycle.clone(),
            intent: Mutex::new(Intent::default()),
        });
        let result = ATTEMPT.scope(context.clone(), work).await;
        *self.intent.lock().unwrap() = std::mem::take(&mut *context.intent.lock().unwrap());
        result
    }
    pub fn has_finish(&self) -> bool {
        let intent = self.intent.lock().unwrap();
        intent.disposition.is_some()
            || self.lifecycle.as_ref().is_some_and(|s| {
                !s.leases.is_empty() && s.leases.iter().any(|l| intent.released.contains(l))
            })
    }
    pub async fn commit(mut self, tx: Transaction<'static, Postgres>) -> Result<(), sqlx::Error> {
        if !self.has_finish() {
            return tx.commit().await;
        }
        let mut tx = tx;
        if let Err(error) = self.validate(&mut tx).await {
            self.rollback(tx).await?;
            return Err(error);
        }
        let lifecycle = self.lifecycle.as_ref().unwrap().clone();
        lifecycle.active()?;
        let disposition = self
            .intent
            .lock()
            .unwrap()
            .disposition
            .unwrap_or(Disposition::Released);
        lifecycle.phase.send_replace(Phase::Committing {
            attempt: self.id,
            disposition,
        });
        self.committing = true;
        lifecycle.record(self.id, disposition, "committing");
        let result = tokio::time::timeout(acknowledgement_timeout(), tx.commit()).await;
        match result {
            Ok(Ok(())) => {
                let released = self.intent.lock().unwrap().released.clone();
                lifecycle.resolve(
                    self.id,
                    Phase::Committed {
                        attempt: self.id,
                        disposition,
                        released,
                    },
                );
                self.committing = false;
                lifecycle.record(self.id, disposition, "committed");
                Ok(())
            }
            Ok(Err(error)) if definite_abort(&error) => {
                lifecycle.resolve(self.id, Phase::Active);
                self.committing = false;
                Err(error)
            }
            outcome => {
                let message = match outcome {
                    Ok(Err(e)) => format!("finalization commit acknowledgement failed: {e}"),
                    _ => "finalization commit acknowledgement timed out".into(),
                };
                lifecycle.resolve(self.id, Phase::RecoveryRequired(message.clone()));
                self.committing = false;
                Err(sqlx::Error::Protocol(message))
            }
        }
    }
    async fn validate(&self, tx: &mut Transaction<'static, Postgres>) -> Result<(), sqlx::Error> {
        let (released, disposition) = {
            let intent = self.intent.lock().unwrap();
            (intent.released.clone(), intent.disposition)
        };
        let root = self.lifecycle.as_ref().unwrap();
        let all_released = root.leases.iter().all(|l| released.contains(l));
        if !all_released
            && (disposition.is_none()
                || root.leases.iter().any(|l| l.resource.starts_with("work:")))
        {
            return Err(sqlx::Error::Protocol(
                "background finalization must release its entire captured resource group".into(),
            ));
        }
        let keys: Vec<_> = self.leases.iter().map(|l| l.resource.clone()).collect();
        let owners: Vec<_> = self.leases.iter().map(|l| l.owner.clone()).collect();
        let works: Vec<_> = self.leases.iter().map(|l| l.work.clone()).collect();
        let generations: Vec<_> = self.leases.iter().map(|l| l.generation).collect();
        let epochs: Vec<_> = self.leases.iter().map(|l| l.epoch.clone()).collect();
        let cleared: Vec<_> = self.leases.iter().map(|l| released.contains(l)).collect();
        let work = self.leases.iter().find(|l| l.resource.starts_with("work:"));
        let job = self.leases.iter().find(|l| l.resource.starts_with("job:"));
        // The intent can outlive a rolled-back SAVEPOINT inside the closure.
        // Validate the actual final rows, with the locks already held by this
        // attempt, rather than promoting an in-memory declaration to success.
        let valid:bool=sqlx::query_scalar(r#"
            WITH requested AS (
                SELECT * FROM unnest($1::text[],$2::text[],$3::text[],$4::bigint[],$5::text[],$6::bool[])
                    r(resource_key,owner,work_id,generation,epoch,cleared)
            )
            SELECT NOT EXISTS(SELECT 1 FROM requested r WHERE NOT EXISTS(
                SELECT 1 FROM resource_leases l WHERE l.resource_key=r.resource_key
                    AND l.generation=r.generation AND l.epoch=r.epoch AND l.epoch=hysteriax_execution_epoch()
                    AND CASE WHEN r.cleared THEN l.owner IS NULL AND l.work_id IS NULL AND l.expires_at IS NULL AND NOT l.blocked
                        ELSE l.owner=r.owner AND l.work_id=r.work_id AND l.expires_at>clock_timestamp() AND (NOT l.blocked OR $10) END
            ))
            AND ($7::text IS NULL OR EXISTS(SELECT 1 FROM background_work WHERE id=$7 AND generation=$8 AND state<>'running' AND owner IS NULL))
            AND ($9::text IS NULL OR EXISTS(SELECT 1 FROM jobs WHERE id=$9 AND status<>'running'))
            AND ($10 OR NOT EXISTS(SELECT 1 FROM execution_operations WHERE (resource_key=ANY($1::text[]) OR work_id=ANY($3::text[])) AND state IN ('dispatching','reconciling','needs_attention')))
        "#).bind(keys).bind(owners).bind(works).bind(generations).bind(epochs).bind(cleared)
            .bind(work.map(|l|l.work.as_str())).bind(work.map(|l|l.generation))
            .bind(job.map(|l|l.work.as_str())).bind(disposition==Some(Disposition::Quarantined))
            .fetch_one(&mut **tx).await?;
        if !valid {
            return Err(sqlx::Error::Protocol(
                "finalization intent does not match the guarded final database rows".into(),
            ));
        }
        Ok(())
    }
    pub async fn rollback(self, tx: Transaction<'static, Postgres>) -> Result<(), sqlx::Error> {
        let result = if self.has_finish() {
            tokio::time::timeout(acknowledgement_timeout(), tx.rollback())
                .await
                .unwrap_or_else(|_| {
                    Err(sqlx::Error::Protocol(
                        "finalization rollback timed out".into(),
                    ))
                })
        } else {
            tx.rollback().await
        };
        if result.is_err()
            && self.has_finish()
            && let Some(lifecycle) = &self.lifecycle
        {
            lifecycle.phase.send_replace(Phase::RecoveryRequired(
                "finalization rollback was not acknowledged".into(),
            ));
        }
        result
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if self.committing
            && let Some(lifecycle) = &self.lifecycle
        {
            lifecycle.resolve(
                self.id,
                Phase::RecoveryRequired(
                    "finalization commit future was dropped before acknowledgement".into(),
                ),
            );
        }
    }
}

fn definite_abort(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|code| !code.starts_with("08") && !code.starts_with("57"))
}

#[cfg(test)]
tokio::task_local! { pub(super) static ACK_TIMEOUT: Duration; }
pub(super) fn acknowledgement_timeout() -> Duration {
    #[cfg(test)]
    if let Ok(timeout) = ACK_TIMEOUT.try_with(|d| *d) {
        return timeout;
    }
    Duration::from_secs(crate::runtime_config::db_statement_timeout_seconds())
}

fn stage(
    connection: &mut PgConnection,
    update: impl FnOnce(&mut Intent, &Lifecycle) -> Result<(), sqlx::Error>,
) -> Result<(), sqlx::Error> {
    let Some(lifecycle) = CURRENT.try_with(|s| s.lifecycle.clone()).ok().flatten() else {
        return Ok(());
    };
    ATTEMPT
        .try_with(|attempt| {
            if attempt.connection != connection as *mut PgConnection as usize
                || !attempt
                    .lifecycle
                    .as_ref()
                    .is_some_and(|s| Arc::ptr_eq(s, &lifecycle))
            {
                return Err(sqlx::Error::Protocol(
                    "finalization intent belongs to another database attempt".into(),
                ));
            }
            update(&mut attempt.intent.lock().unwrap(), &lifecycle)
        })
        .unwrap_or_else(|_| {
            Err(sqlx::Error::Protocol(
                "supervised finalization requires the database transaction boundary".into(),
            ))
        })
}

pub(super) fn released(
    connection: &mut PgConnection,
    lease: &Execution,
) -> Result<(), sqlx::Error> {
    stage(connection, |intent, _| {
        // Record child releases too, but has_finish only completes the root
        // execution when its primary transitions or all root leases release.
        if !intent.released.contains(lease) {
            intent.released.push(lease.clone());
        }
        Ok(())
    })
}

pub(crate) fn finish_on(
    connection: &mut PgConnection,
    primary: &str,
    disposition: Disposition,
) -> Result<(), sqlx::Error> {
    stage(connection, |intent, lifecycle| {
        if !lifecycle.leases.iter().any(|l| l.resource == primary) {
            return Err(sqlx::Error::Protocol(
                "finalization intent does not belong to this execution".into(),
            ));
        }
        if intent.disposition.is_none() {
            intent.disposition = Some(disposition);
        }
        Ok(())
    })
}

pub(super) fn ensure_active() -> Result<(), sqlx::Error> {
    CURRENT
        .try_with(|s| s.lifecycle.as_ref().map_or(Ok(()), |l| l.active()))
        .unwrap_or(Ok(()))
}

pub(super) fn validate_release_scope(
    connection: &mut PgConnection,
    lease: &Execution,
) -> Result<(), sqlx::Error> {
    CURRENT
        .try_with(|scope| {
            if let Some(lifecycle) = &scope.lifecycle {
                lifecycle.active()?;
                if !scope.leases.contains(lease) {
                    return Err(sqlx::Error::Protocol(
                        "lease release is outside the captured execution scope".into(),
                    ));
                }
            }
            Ok(())
        })
        .unwrap_or(Ok(()))?;
    require_attempt(connection)
}

pub(crate) fn require_attempt(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    stage(connection, |_, _| Ok(()))
}
