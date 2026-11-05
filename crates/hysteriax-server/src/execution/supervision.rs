use super::finalization::{Lifecycle, Phase};
use super::{CURRENT, Execution, RENEW_SECONDS, Scope, check_current, observe_current};
use sqlx::PgPool;
use std::{future::Future, sync::Arc, time::Duration};

#[derive(Debug, PartialEq, Eq)]
pub enum Supervised<T> {
    Returned(T),
    Lost(String),
    RecoveryRequired(String),
}
impl<T> Supervised<T> {
    /// Expected cancellation has no value. Uncertain publication is an error,
    /// never the same absence of work that older Option callers accepted.
    pub fn into_result(self) -> Result<Option<T>, sqlx::Error> {
        match self {
            Self::Returned(value) => Ok(Some(value)),
            Self::Lost(_) => Ok(None),
            Self::RecoveryRequired(reason) => Err(sqlx::Error::Protocol(reason)),
        }
    }
    #[cfg(test)]
    pub fn unwrap(self) -> T {
        match self {
            Self::Returned(v) => v,
            other => panic!(
                "execution did not return: {}",
                match other {
                    Self::Lost(s) | Self::RecoveryRequired(s) => s,
                    _ => unreachable!(),
                }
            ),
        }
    }
    #[cfg(test)]
    pub fn is_none(&self) -> bool {
        matches!(self, Self::Lost(_))
    }
    #[cfg(test)]
    pub fn is_some(&self) -> bool {
        matches!(self, Self::Returned(_))
    }
}

pub async fn supervise<T>(
    pool: PgPool,
    leases: Vec<Execution>,
    work: impl Future<Output = T>,
) -> Supervised<T> {
    let lifecycle = Some(Lifecycle::new(leases.clone()));
    let scope = Scope {
        pool,
        leases,
        lifecycle,
        sequence: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };
    supervise_scope(scope, Box::pin(work)).await
}

#[cfg(test)]
tokio::task_local! {pub(super) static RENEW_INTERVAL: Duration;}
async fn renew(scope: Scope) -> String {
    #[cfg(test)]
    let interval = RENEW_INTERVAL
        .try_with(|d| *d)
        .unwrap_or(Duration::from_secs(RENEW_SECONDS));
    #[cfg(not(test))]
    let interval = Duration::from_secs(RENEW_SECONDS);
    loop {
        let mut contended = false;
        for lease in &scope.leases {
            match lease.renew(&scope.pool).await {
                Ok(true) => {}
                Err(error)
                    if error.as_database_error().and_then(|e| e.code()).as_deref()
                        == Some("55P03") =>
                {
                    contended = true
                }
                result => {
                    return format!(
                        "execution renewal lost: resource={} owner={} generation={} epoch={} result={result:?}",
                        lease.resource, lease.owner, lease.generation, lease.epoch
                    );
                }
            }
        }
        if let Err(error) = observe_current(&scope).await {
            return format!("execution cancelled or ownership expired: {error}");
        }
        tokio::time::sleep(if contended {
            Duration::from_millis(250)
        } else {
            interval
        })
        .await;
    }
}

pub(super) async fn supervise_scope<T>(
    scope: Scope,
    work: impl Future<Output = T>,
) -> Supervised<T> {
    #[cfg(test)]
    let measured_lifecycle = scope.lifecycle.clone();
    let work = Box::pin(work);
    let outcome=CURRENT.scope(scope.clone(),async move {
        if let Err(error)=check_current().await {
            let reason=format!("execution ownership was lost before work started: {error}");
            if let Some(l)=&scope.lifecycle {l.lose(reason.clone());}
            tracing::warn!(%reason,"execution stopped");
            return Supervised::Lost(reason);
        }
        let lifecycle=scope.lifecycle.as_ref().expect("supervised scope has a lifecycle").clone();
        let mut changed=lifecycle.subscribe();
        let mut renewal:std::pin::Pin<Box<dyn Future<Output=String>+Send>>=Box::pin(renew(scope.clone()));
        let mut monitoring=true;
        let mut phase_deadline=None;
        let mut deadline_phase=0;
        tokio::pin!(work);
        loop {
            match lifecycle.phase() {
                Phase::Active=>{
                    phase_deadline=None;deadline_phase=0;
                    if !monitoring {renewal=Box::pin(renew(scope.clone()));monitoring=true;}
                },
                Phase::Committing{attempt,disposition}=>{
                    let _=(attempt,disposition);
                    // Drop the in-flight renewal, but keep polling the commit
                    // future. A rollback installs a fresh renewal/observation.
                    if monitoring {renewal=Box::pin(std::future::pending());monitoring=false;}
                    if deadline_phase!=1 {phase_deadline=Some(tokio::time::Instant::now()+super::finalization::acknowledgement_timeout());deadline_phase=1;}
                },
                Phase::Committed{attempt,disposition,..}=>{
                    tracing::trace!(attempt,?disposition,"execution returning after committed finalization");
                    if monitoring {renewal=Box::pin(std::future::pending());monitoring=false;}
                    if deadline_phase!=2 {phase_deadline=Some(tokio::time::Instant::now()+super::finalization::acknowledgement_timeout());deadline_phase=2;}
                },
                Phase::RecoveryRequired(reason)=>{
                    tracing::warn!(%reason,"execution finalization requires recovery");
                    return Supervised::RecoveryRequired(reason);
                },
                Phase::Lost(reason)=>return Supervised::Lost(reason),
            }
            tokio::select! {
                biased;
                _=changed.changed()=>{},
                reason=&mut renewal,if monitoring=>{
                    // The database may have committed while renewal was waiting
                    // on this execution's row lock. Re-read the local handoff
                    // before treating its zero-row result as a takeover.
                    if matches!(lifecycle.phase(),Phase::Active) {
                        lifecycle.lose(reason.clone());
                        tracing::warn!(%reason,"execution stopped");
                        return Supervised::Lost(reason);
                    }
                    monitoring=false;
                },
                output=&mut work=>{
                    return match lifecycle.phase() {
                        Phase::RecoveryRequired(reason)=>Supervised::RecoveryRequired(reason),
                        Phase::Lost(reason)=>Supervised::Lost(reason),
                        _=>Supervised::Returned(output),
                    };
                },
                _=async {tokio::time::sleep_until(phase_deadline.unwrap()).await},if phase_deadline.is_some()=>{
                    let reason="execution finalization continuation timed out".to_owned();
                    lifecycle.require_recovery(reason.clone());
                    return Supervised::RecoveryRequired(reason);
                }
            }
        }
    }).await;
    #[cfg(test)]
    counts::record(
        &outcome,
        measured_lifecycle
            .as_ref()
            .is_some_and(|l| matches!(l.phase(), Phase::Committed { .. })),
    );
    outcome
}

#[cfg(test)]
pub(super) mod counts {
    use super::Supervised;
    use std::sync::atomic::{AtomicU64, Ordering};
    static RETURNED: AtomicU64 = AtomicU64::new(0);
    static COMMITTED: AtomicU64 = AtomicU64::new(0);
    static LOST: AtomicU64 = AtomicU64::new(0);
    static RECOVERY: AtomicU64 = AtomicU64::new(0);
    pub fn record<T>(outcome: &Supervised<T>, committed: bool) {
        match outcome {
            Supervised::Returned(_) => {
                RETURNED.fetch_add(1, Ordering::Relaxed);
                if committed {
                    COMMITTED.fetch_add(1, Ordering::Relaxed);
                }
            }
            Supervised::Lost(_) => {
                LOST.fetch_add(1, Ordering::Relaxed);
            }
            Supervised::RecoveryRequired(_) => {
                RECOVERY.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    pub fn reset() {
        for value in [&RETURNED, &COMMITTED, &LOST, &RECOVERY] {
            value.store(0, Ordering::SeqCst);
        }
    }
    pub fn snapshot() -> serde_json::Value {
        serde_json::json!({"returned":RETURNED.load(Ordering::SeqCst),"committed_returned":COMMITTED.load(Ordering::SeqCst),"lost":LOST.load(Ordering::SeqCst),"recovery_required":RECOVERY.load(Ordering::SeqCst)})
    }
}
