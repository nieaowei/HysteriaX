use super::CURRENT;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteStep {
    pub operation_id: String,
    pub resource: String,
    pub owner: String,
    pub generation: i64,
    pub epoch: String,
    pub work: String,
    pub step: String,
    pub kind: String,
    pub request_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predecessor: Option<String>,
}

/// Read the immutable task-local resource before preparing remote metadata.
pub(crate) fn resource_for(kind: &str) -> anyhow::Result<Option<super::Execution>> {
    let Ok(scope) = CURRENT.try_with(Clone::clone) else {
        return Ok(None);
    };
    let prefix = if kind.starts_with("dns") {
        "dns-"
    } else {
        "node:"
    };
    let lease = scope
        .leases
        .iter()
        .find(|l| l.resource.starts_with(prefix))
        .or_else(|| scope.leases.first())
        .ok_or_else(|| anyhow::anyhow!("remote work requires a resource lease"))?;
    Ok(Some(lease.clone()))
}

pub async fn run<T>(
    kind: &str,
    digest: String,
    request: impl std::future::Future<Output = anyhow::Result<T>>,
    evidence: impl FnOnce(&T) -> Value,
) -> anyhow::Result<T> {
    with_context(kind, digest, Value::Null, move |_| request, evidence).await
}

/// The factory itself runs only after the durable intent has committed.
pub(crate) async fn with_context<T, F: std::future::Future<Output = anyhow::Result<T>>>(
    kind: &str,
    digest: String,
    verification: Value,
    request: impl FnOnce(Option<RemoteStep>) -> F,
    evidence: impl FnOnce(&T) -> Value,
) -> anyhow::Result<T> {
    let Some(scope) = CURRENT.try_with(Clone::clone).ok() else {
        // Only historical unit fixtures may bypass leased remote execution.
        #[cfg(test)]
        return request(None).await;
        #[cfg(not(test))]
        anyhow::bail!("mutating remote work requires an execution scope");
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
        .ok_or_else(|| anyhow::anyhow!("remote work requires a resource lease"))?
        .clone();
    let index = scope
        .sequence
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let step = format!("{kind}:{index}");
    let predecessor = verification
        .get("expected_head")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut intent = serde_json::json!({"kind":kind,"request_digest":digest});
    if !verification.is_null() {
        intent["verification"] = verification;
    }
    let job = scope
        .leases
        .iter()
        .find(|l| l.resource.starts_with("job:"))
        .map(|l| l.work.clone());
    let prepared_id = uuid::Uuid::new_v4().to_string();
    let (operation,fresh)=crate::db::write(&scope.pool,|tx| {
        let (lease,job,step,intent,id)=(lease.clone(),job.clone(),step.clone(),intent.clone(),prepared_id.clone());
        Box::pin(async move {
            if let Some(job)=job {
                let outcome:Option<(Option<String>,)>=sqlx::query_as("SELECT r.outcome FROM mtls_node_operations o JOIN mtls_rotations r ON r.id=o.rotation_id WHERE o.job_id=$1 FOR KEY SHARE OF r")
                    .bind(job).fetch_optional(&mut **tx).await?;
                anyhow::ensure!(outcome.is_none_or(|(outcome,)|outcome.is_none()),"CA target was superseded before dispatch");
            }
            Ok::<_,anyhow::Error>(lease.dispatch_prepared(tx,&step,&intent,&id).await?)
        })
    }).await?;
    anyhow::ensure!(
        fresh,
        "operation was previously dispatched; remote verification is required before recovery"
    );
    let context = RemoteStep {
        operation_id: operation.clone(),
        resource: lease.resource.clone(),
        owner: lease.owner.clone(),
        generation: lease.generation,
        epoch: lease.epoch.clone(),
        work: lease.work.clone(),
        step,
        kind: kind.into(),
        request_digest: digest,
        predecessor,
    };
    let output = request(Some(context)).await?;
    let evidence = evidence(&output);
    crate::db::write(&scope.pool, |tx| {
        let (lease, operation, evidence) = (lease.clone(), operation.clone(), evidence.clone());
        Box::pin(async move {
            lease.verified(tx, &operation, &evidence).await?;
            Ok::<_, anyhow::Error>(())
        })
    })
    .await?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{self, CURRENT, Scope};
    use sqlx::Row;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test]
    async fn factory_receives_committed_context_and_is_not_reinvoked_for_an_uncertain_step() {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        let mut tx = pool.begin().await.unwrap();
        let lease = execution::acquire(&mut tx, "node:factory", "owner", "work")
            .await
            .unwrap()
            .unwrap();
        tx.commit().await.unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        for attempt in 0..2 {
            let scope = Scope {
                pool: pool.clone(),
                leases: vec![lease.clone()],
                sequence: Default::default(),
            };
            let (calls, pool) = (count.clone(), pool.clone());
            let result: anyhow::Result<()> = CURRENT.scope(scope,with_context("ssh-command","digest".into(),serde_json::json!({"protocol":"test"}),move |context| {
                calls.fetch_add(1,Ordering::SeqCst);
                async move {
                    let c=context.unwrap();
                    let row=sqlx::query("SELECT resource_key,owner,generation,epoch,work_id,step,state,intent FROM execution_operations WHERE operation_id=$1").bind(&c.operation_id).fetch_one(&pool).await?;
                    assert_eq!(row.get::<String,_>("state"),"dispatching");
                    assert_eq!(row.get::<String,_>("resource_key"),c.resource);assert_eq!(row.get::<String,_>("epoch"),c.epoch);
                    assert_eq!(row.get::<String,_>("owner"),c.owner);assert_eq!(row.get::<i64,_>("generation"),c.generation);
                    assert_eq!(row.get::<String,_>("work_id"),c.work);assert_eq!(row.get::<String,_>("step"),c.step);
                    assert_eq!(row.get::<Value,_>("intent"),serde_json::json!({"kind":"ssh-command","request_digest":"digest","verification":{"protocol":"test"}}));
                    anyhow::bail!("lost response")
                }
            },|_|Value::Null)).await;
            assert!(result.is_err());
            assert_eq!(count.load(Ordering::SeqCst), 1, "attempt {attempt}");
        }
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use crate::execution::{self, CURRENT, Scope};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test]
    async fn native_intent_and_receipt_retries_never_reinvoke_the_network_factory() {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        let mut tx = pool.begin().await.unwrap();
        let lease = execution::acquire(&mut tx, "node:retries", "owner", "work")
            .await
            .unwrap()
            .unwrap();
        tx.commit().await.unwrap();
        sqlx::raw_sql("CREATE SEQUENCE intent_attempts; CREATE SEQUENCE receipt_attempts; CREATE SEQUENCE operation_binding MINVALUE 0 START 0; CREATE FUNCTION retry_remote_intent() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('intent_attempts')=1 THEN PERFORM setval('operation_binding',hashtextextended(NEW.operation_id,0) & 9223372036854775807,true); RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry intent'; END IF; IF (hashtextextended(NEW.operation_id,0) & 9223372036854775807)<>(SELECT last_value FROM operation_binding) THEN RAISE EXCEPTION 'operation ID changed on retry'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_remote_intent BEFORE INSERT ON execution_operations FOR EACH ROW EXECUTE FUNCTION retry_remote_intent(); CREATE FUNCTION retry_remote_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='verified' AND nextval('receipt_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40P01',MESSAGE='retry receipt'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_remote_receipt BEFORE UPDATE ON execution_operations FOR EACH ROW EXECUTE FUNCTION retry_remote_receipt();")
            .execute(&pool).await.unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let scope = Scope {
            pool: pool.clone(),
            leases: vec![lease],
            sequence: Default::default(),
        };
        let result = CURRENT
            .scope(
                scope,
                with_context(
                    "ssh-command",
                    "digest".into(),
                    serde_json::json!({"protocol":"test"}),
                    move |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        async { Ok(42) }
                    },
                    |v| serde_json::json!({"value":v}),
                ),
            )
            .await
            .unwrap();
        assert_eq!(result, 42);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM execution_operations WHERE state='verified'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT last_value FROM intent_attempts")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT last_value FROM receipt_attempts")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
    }
}

#[cfg(test)]
mod predecessor_tests {
    use super::*;
    use crate::execution::{self, CURRENT, Scope};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test]
    async fn obsolete_scope_cannot_use_a_fresh_predecessor_or_construct_a_payload() {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        let mut tx = pool.begin().await.unwrap();
        let lease = execution::acquire(&mut tx, "node:old-head", "owner", "work")
            .await
            .unwrap()
            .unwrap();
        tx.commit().await.unwrap();
        let epoch = uuid::Uuid::new_v4().to_string();
        sqlx::query("UPDATE cache_version_epoch SET epoch=$1 WHERE id")
            .bind(&epoch)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE resource_leases SET epoch=$1")
            .bind(&epoch)
            .execute(&pool)
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let scope = Scope {
            pool: pool.clone(),
            leases: vec![lease],
            sequence: Default::default(),
        };
        let result: anyhow::Result<()> = CURRENT
            .scope(
                scope,
                with_context(
                    "ssh-command",
                    "digest".into(),
                    serde_json::json!({"expected_head":uuid::Uuid::new_v4().to_string()}),
                    move |_| {
                        count.fetch_add(1, Ordering::SeqCst);
                        async { Ok(()) }
                    },
                    |_| Value::Null,
                ),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM execution_operations")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }
}
