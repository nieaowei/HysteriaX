use super::*;
use serde_json::json;

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}
async fn external(pool: &PgPool, key: &str) -> Execution {
    let key = key.to_owned();
    db::write(pool, move |tx| {
        let key = key.clone();
        Box::pin(async move {
            Ok::<_, anyhow::Error>(
                execution::acquire(tx, &key, "external", "external-work")
                    .await?
                    .unwrap(),
            )
        })
    })
    .await
    .unwrap()
}
async fn complete(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::write(pool, move |tx| {
        let work = work.clone();
        Box::pin(async move { work.finish_on(tx, false, 60, None).await })
    })
    .await
}

#[tokio::test]
async fn busy_scopes_are_excluded_before_candidate_limit_and_share_node_leases_with_jobs() {
    let pool = db::test_pool_with_max_connections(8).await;
    let external = external(&pool, "ca:busy").await;
    for i in 0..40 {
        ensure(
            &pool,
            &format!("scope-busy-{i:03}"),
            "scoped-test",
            json!({"lease_resources":["ca:busy"]}),
        )
        .await
        .unwrap();
    }
    ensure(
        &pool,
        "scope-z",
        "scoped-test",
        json!({"lease_resources":["node:b","ca:b","node:b"]}),
    )
    .await
    .unwrap();
    let work = claim(&pool, "scoped-test", "owner").await.unwrap().unwrap();
    assert_eq!(work.id, "maintenance:scope-z");
    assert_eq!(
        work.leases
            .iter()
            .map(|l| l.resource.as_str())
            .collect::<Vec<_>>(),
        vec!["ca:b", "node:b", "work:maintenance:scope-z"]
    );
    let attempt = db::write(&pool, |tx| {
        Box::pin(async move {
            Ok::<_, anyhow::Error>(execution::acquire(tx, "node:b", "job-owner", "node-job").await?)
        })
    })
    .await
    .unwrap();
    assert!(attempt.is_none());
    assert!(external.renew(&pool).await.unwrap());
    complete(&pool, &work).await.unwrap();
    assert_eq!(count(&pool,"SELECT count(*) FROM resource_leases WHERE resource_key IN ('ca:b','node:b','work:maintenance:scope-z') AND owner IS NULL").await,3);
    assert!(
        claim(&pool, "scoped-test", "other")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn partial_resource_claim_rolls_back_all_generations_and_continues_with_another_candidate() {
    let pool = db::test_pool_with_max_connections(8).await;
    sqlx::query(
        "INSERT INTO resource_leases(resource_key,generation) VALUES('ca:a',7),('node:a',11)",
    )
    .execute(&pool)
    .await
    .unwrap();
    ensure(
        &pool,
        "a",
        "scoped-test",
        json!({"lease_resources":["node:a","ca:a"]}),
    )
    .await
    .unwrap();
    ensure(
        &pool,
        "b",
        "scoped-test",
        json!({"lease_resources":["ca:b"]}),
    )
    .await
    .unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT resource_key FROM resource_leases WHERE resource_key='node:a' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let work = claim(&pool, "scoped-test", "owner").await.unwrap().unwrap();
    assert_eq!(work.id, "maintenance:b");
    assert_eq!(
        count(
            &pool,
            "SELECT generation FROM resource_leases WHERE resource_key='ca:a'"
        )
        .await,
        7
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE resource_key='ca:a' AND owner IS NULL"
        )
        .await,
        1
    );
    assert_eq!(count(&pool,"SELECT count(*) FROM background_work WHERE id='maintenance:a' AND state='queued' AND owner IS NULL").await,1);
    held.rollback().await.unwrap();
    complete(&pool, &work).await.unwrap();
    let next = claim(&pool, "scoped-test", "next").await.unwrap().unwrap();
    assert_eq!(next.id, "maintenance:a");
    assert_eq!(
        count(
            &pool,
            "SELECT generation FROM resource_leases WHERE resource_key='ca:a'"
        )
        .await,
        8
    );
    assert_eq!(
        count(
            &pool,
            "SELECT generation FROM resource_leases WHERE resource_key='node:a'"
        )
        .await,
        12
    );
    complete(&pool, &next).await.unwrap();
}

#[tokio::test]
async fn takeover_uses_the_work_generation_even_when_auxiliary_generations_differ_and_fences_all_progress()
 {
    let pool = db::test_pool_with_max_connections(8).await;
    sqlx::query(
        "INSERT INTO resource_leases(resource_key,generation) VALUES('ca:a',80),('identity:a',40)",
    )
    .execute(&pool)
    .await
    .unwrap();
    ensure(
        &pool,
        "a",
        "scoped-test",
        json!({"lease_resources":["identity:a","ca:a"]}),
    )
    .await
    .unwrap();
    let old = claim(&pool, "scoped-test", "old").await.unwrap().unwrap();
    assert_eq!(old.primary().unwrap().generation, 1);
    assert_eq!(old.leases[0].generation, 81);
    assert_eq!(
        count(
            &pool,
            "SELECT generation FROM background_work WHERE id='maintenance:a'"
        )
        .await,
        1
    );
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&pool).await.unwrap();
    let next = claim(&pool, "scoped-test", "next").await.unwrap().unwrap();
    assert_eq!(next.primary().unwrap().generation, 2);
    assert_eq!(next.leases[0].generation, 82);
    assert!(complete(&pool, &old).await.is_err());
    let rejected = execution::supervise(pool.clone(), old.leases.clone(), async {
        panic!("old scoped work was polled")
    })
    .await;
    assert!(rejected.is_none());
    let result =
        execution::supervise(pool.clone(), next.leases.clone(), complete(&pool, &next)).await;
    result.unwrap().unwrap();
    assert_eq!(count(&pool,"SELECT count(*) FROM resource_leases WHERE work_id='maintenance:a' AND owner IS NOT NULL").await,0);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM background_work WHERE state='complete' AND generation=2"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn failed_scoped_ack_keeps_every_resource_owned_until_an_atomic_retry() {
    let pool = db::test_pool_with_max_connections(8).await;
    ensure(
        &pool,
        "a",
        "scoped-test",
        json!({"lease_resources":["node:a","ca:a"]}),
    )
    .await
    .unwrap();
    let work = claim(&pool, "scoped-test", "owner").await.unwrap().unwrap();
    let aborted: Result<()> = db::write(&pool, |tx| {
        let work = work.clone();
        Box::pin(async move {
            work.finish_on(tx, false, 60, None).await?;
            anyhow::bail!("rollback all scope releases and confirmation")
        })
    })
    .await;
    assert!(aborted.is_err());
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE owner='owner' AND work_id='maintenance:a'"
        )
        .await,
        3
    );
    for lease in &work.leases {
        assert!(lease.renew(&pool).await.unwrap());
    }
    complete(&pool, &work).await.unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE owner='owner'"
        )
        .await,
        0
    );
}
