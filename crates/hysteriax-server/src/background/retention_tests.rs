use super::*;
use crate::security::SecretBox;
use axum::{Json, extract::State};
use chrono::TimeDelta;
use serde_json::json;
use sqlx::PgPool;

async fn fixture() -> (AppState, String, DateTime<Utc>) {
    let pool = db::test_pool_with_max_connections(8).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    let state = AppState::new(pool, secrets);
    let request = crate::credentials::test_node_request(&state, json!({
        "name":"Retention node","ssh_host":"127.0.0.1","ssh_port":1,"ssh_username":"root",
        "ssh_auth_type":"password","ssh_secret":"test","public_host":"example.test","public_port":443,
        "listen_addr":":443"
    })).await;
    let (_, Json(receipt)) = crate::api::nodes::create(State(state.clone()), Json(request))
        .await
        .unwrap();
    let cutoff = sqlx::query_scalar("SELECT clock_timestamp() - interval '14 days'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    (
        state,
        receipt["node"]["id"].as_str().unwrap().into(),
        cutoff,
    )
}

async fn cycle(pool: &PgPool, kind: &str, ended: DateTime<Utc>) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sampling_cycles (id,kind,period_start,deadline_at,expected_nodes)
         VALUES ($1,$2,$3 - interval '1 minute',$3,1)",
    )
    .bind(id)
    .bind(kind)
    .bind(ended)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn work(
    pool: &PgPool,
    node: &str,
    cycle: Uuid,
    kind: &str,
    ended: DateTime<Utc>,
    state: &str,
) -> String {
    let id = format!("{cycle}:{node}");
    sqlx::query(
        "INSERT INTO background_work (id,cycle_id,node_id,kind,state,deadline_at,finished_at,owner,generation)
         VALUES ($1,$2,$3,$4,$5,$6,$6,CASE WHEN $5='running' THEN 'active' END,1)",
    ).bind(&id).bind(cycle).bind(node).bind(kind).bind(state).bind(ended).execute(pool).await.unwrap();
    id
}

async fn batch(pool: &PgPool, node: &str, work: &str, cycle: Uuid, state: &str, at: DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO traffic_sample_batches (sample_id,node_id,cycle_id,state,completed_at,sampled_at,payload_digest)
         VALUES ($1,$2,$3,$4,CASE WHEN $4 IN ('discarded','complete') THEN $5 END,$5,
             CASE WHEN $4 IN ('ready','complete') THEN 'captured' END)",
    ).bind(work).bind(node).bind(cycle).bind(state).bind(at).execute(pool).await.unwrap();
}

async fn exists(pool: &PgPool, query: &str, id: &str) -> bool {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn deletion_preserves_billing_and_monitor_history() {
    let (state, node, cutoff) = fixture().await;
    let pool = &state.pool;
    let mut work_ids = BTreeMap::new();
    for (index, name) in [
        "discarded",
        "complete",
        "ready",
        "clear",
        "pending",
        "evidence",
        "blocked",
        "attached",
        "running",
        "boundary",
    ]
    .into_iter()
    .enumerate()
    {
        let ended = if name == "boundary" {
            cutoff
        } else {
            cutoff - TimeDelta::hours(index as i64 + 2)
        };
        let cycle = cycle(pool, "traffic", ended).await;
        let status = if name == "running" {
            "running"
        } else {
            "complete"
        };
        work_ids.insert(
            name,
            (
                work(pool, &node, cycle, "traffic", ended, status).await,
                cycle,
            ),
        );
    }
    let (discarded, discard_cycle) = &work_ids["discarded"];
    let (complete, complete_cycle) = &work_ids["complete"];
    let (ready, ready_cycle) = &work_ids["ready"];
    let (clear, clear_cycle) = &work_ids["clear"];
    let (_, pending_cycle) = &work_ids["pending"];
    let (evidence, _) = &work_ids["evidence"];
    let (blocked, _) = &work_ids["blocked"];
    let (attached, _) = &work_ids["attached"];
    let date = cutoff - TimeDelta::hours(1);
    batch(pool, &node, discarded, *discard_cycle, "discarded", date).await;
    batch(pool, &node, complete, *complete_cycle, "complete", date).await;
    batch(pool, &node, ready, *ready_cycle, "ready", date).await;
    batch(pool, &node, clear, *clear_cycle, "discarded", date).await;
    sqlx::query("UPDATE traffic_sample_batches SET clear_requested_at=$2,clear_owner='prior',clear_work_generation=1,clear_node_generation=1,clear_epoch=hysteriax_execution_epoch() WHERE sample_id=$1")
        .bind(clear).bind(date).execute(pool).await.unwrap();
    sqlx::query(
        "UPDATE traffic_sample_batches SET item_count=1,processed_count=1 WHERE sample_id=$1",
    )
    .bind(complete)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO traffic_sample_items (sample_id,node_id,user_id,tx_total,rx_total,processed_at,sampled_at) VALUES ($1,$2,'historical-user',2,3,$3,$3)")
        .bind(complete).bind(&node).bind(date).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO online_samples (cycle_id,node_id,sampled_at,status,connections,cycle_started_at,expected_nodes,pending) VALUES ($1,$3,$4,'failed','{}',$4,1,false), ($2,$3,$4,'failed','{}',$4,1,true)")
        .bind(complete_cycle).bind(pending_cycle).bind(&node).bind(date).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO resource_leases (resource_key,generation,blocked) VALUES ('work:'||$1,1,false),('work:'||$2,1,false),('work:'||$3,1,true)")
        .bind(discarded).bind(evidence).bind(blocked).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO resource_leases (resource_key,owner,work_id,generation,expires_at) VALUES ('node:'||$1,'old',$2,1,$3)")
        .bind(&node).bind(attached).bind(date).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO execution_operations (operation_id,resource_key,owner,generation,work_id,step,state,intent) VALUES ('retained','work:'||$1,'old',1,$1,'read','verified','{}')")
        .bind(evidence).execute(pool).await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    let deleted = scan_work(&mut tx, cutoff, &mut Cursor::default())
        .await
        .unwrap();
    assert_eq!(
        (
            deleted.work_deleted,
            deleted.leases_deleted,
            deleted.batches_deleted
        ),
        (2, 1, 1)
    );
    assert_eq!(deleted.eligible, 2);
    assert_eq!(deleted.skipped["traffic_batch_retained"], 2);
    assert_eq!(deleted.skipped["lease_retained"], 2);
    assert_eq!(deleted.skipped["execution_evidence"], 1);
    assert_eq!(deleted.skipped["online_pending"], 1);
    let cycles = scan_cycles(&mut tx, cutoff, &mut Cursor::default())
        .await
        .unwrap();
    assert_eq!(cycles.cycles_deleted, 2);
    tx.commit().await.unwrap();

    for (name, (id, cycle)) in &work_ids {
        let kept = !["discarded", "complete"].contains(name);
        assert_eq!(
            exists(
                pool,
                "SELECT EXISTS(SELECT 1 FROM background_work WHERE id=$1)",
                id
            )
            .await,
            kept,
            "{name}"
        );
        assert_eq!(
            exists(
                pool,
                "SELECT EXISTS(SELECT 1 FROM sampling_cycles WHERE id=$1::text::uuid)",
                &cycle.to_string()
            )
            .await,
            kept,
            "{name}"
        );
    }
    assert!(exists(pool, "SELECT EXISTS(SELECT 1 FROM traffic_sample_items WHERE sample_id=$1 AND tx_total=2 AND rx_total=3 AND processed_at IS NOT NULL)", complete).await);
    assert!(exists(pool, "SELECT EXISTS(SELECT 1 FROM online_samples WHERE cycle_id=$1::text::uuid AND NOT pending)", &complete_cycle.to_string()).await);
    assert!(
        exists(
            pool,
            "SELECT EXISTS(SELECT 1 FROM online_samples WHERE cycle_id=$1::text::uuid AND pending)",
            &pending_cycle.to_string()
        )
        .await
    );
    assert!(
        !exists(
            pool,
            "SELECT EXISTS(SELECT 1 FROM resource_leases WHERE resource_key='work:'||$1)",
            discarded
        )
        .await
    );
    assert!(exists(pool, "SELECT EXISTS(SELECT 1 FROM execution_operations WHERE work_id=$1 AND state='verified')", evidence).await);
}

#[tokio::test]
async fn locked_old_rows_do_not_block_later_pages_and_are_revisited() {
    let (state, node, cutoff) = fixture().await;
    let pool = &state.pool;
    let ended = cutoff - TimeDelta::hours(2);
    let first = cycle(pool, "proxy", ended).await;
    let first_id = work(pool, &node, first, "proxy", ended, "complete").await;
    sqlx::query("INSERT INTO resource_leases (resource_key,generation) VALUES ('work:'||$1,1)")
        .bind(&first_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO background_work (id, cycle_id,node_id,kind,state,deadline_at,finished_at)
         SELECT $1::uuid::text||':other-'||i, $1, 'other-'||i, 'proxy', 'complete', $2, $2
         FROM generate_series(1, 520) i",
    )
    .bind(first)
    .bind(ended + TimeDelta::seconds(1))
    .execute(pool)
    .await
    .unwrap();
    let mut locker = pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM resource_leases WHERE resource_key='work:'||$1 FOR UPDATE")
        .bind(&first_id)
        .execute(&mut *locker)
        .await
        .unwrap();

    let mut cursor = Cursor::default();
    for (scanned, removed) in [(512, 511), (9, 9)] {
        let mut tx = pool.begin().await.unwrap();
        let counts = scan_work(&mut tx, cutoff, &mut cursor).await.unwrap();
        assert_eq!((counts.scanned, counts.work_deleted), (scanned, removed));
        tx.commit().await.unwrap();
    }
    assert!(cursor.done);
    locker.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let again = scan_work(&mut tx, cutoff, &mut Cursor::default())
        .await
        .unwrap();
    assert_eq!(
        (again.scanned, again.work_deleted, again.leases_deleted),
        (1, 1, 1)
    );
    tx.rollback().await.unwrap();
    assert!(
        exists(
            pool,
            "SELECT EXISTS(SELECT 1 FROM background_work WHERE id=$1)",
            &first_id
        )
        .await
    );
    assert!(
        exists(
            pool,
            "SELECT EXISTS(SELECT 1 FROM resource_leases WHERE resource_key='work:'||$1)",
            &first_id
        )
        .await
    );
}

#[tokio::test]
async fn recurring_worker_retries_a_rolled_back_page_and_does_not_duplicate_claims() {
    let (state, node, cutoff) = fixture().await;
    let pool = &state.pool;
    let ended = cutoff - TimeDelta::hours(2);
    let cycle = cycle(pool, "network", ended).await;
    let id = work(pool, &node, cycle, "network", ended, "complete").await;
    sqlx::raw_sql(
        "CREATE FUNCTION reject_retention_delete() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN IF OLD.cycle_id IS NOT NULL THEN RAISE EXCEPTION 'test rollback'; END IF; RETURN OLD; END $$;
         CREATE TRIGGER reject_retention_delete BEFORE DELETE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_retention_delete();",
    ).execute(pool).await.unwrap();
    assert!(pass(&state).await.is_err());
    assert!(
        exists(
            pool,
            "SELECT EXISTS(SELECT 1 FROM background_work WHERE id=$1)",
            &id
        )
        .await
    );
    let status: (String, bool) = sqlx::query_as(
        "SELECT state,payload='null'::jsonb FROM background_work WHERE kind='sampling-retention'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(status, ("queued".into(), true));
    sqlx::raw_sql("DROP TRIGGER reject_retention_delete ON background_work;
        UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE kind='sampling-retention';")
        .execute(pool).await.unwrap();
    let mut second = state.clone();
    second.instance_id = Uuid::new_v4().to_string();
    second.maintenance = std::sync::Arc::new(maintenance::Scheduler::new(pool.clone()));
    let (first, second) = tokio::join!(pass(&state), pass(&second));
    first.unwrap();
    second.unwrap();
    assert!(
        !exists(
            pool,
            "SELECT EXISTS(SELECT 1 FROM background_work WHERE id=$1)",
            &id
        )
        .await
    );
    let (status, generation, delay): (String, i64, f64) = sqlx::query_as(
        "SELECT state,generation,extract(epoch FROM available_at-finished_at)::float8
         FROM background_work WHERE kind='sampling-retention'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        (status.as_str(), generation, delay.round() as i64),
        ("complete", 2, 300)
    );
}

#[tokio::test]
async fn legacy_observation_cursor_is_restarted_before_deleting() {
    let (state, node, cutoff) = fixture().await;
    let pool = &state.pool;
    let ended = cutoff - TimeDelta::hours(2);
    let cycle_id = cycle(pool, "proxy", ended).await;
    let work_id = work(pool, &node, cycle_id, "proxy", ended, "complete").await;
    let mut sweep = Sweep::new(cutoff);
    sweep.checkpoint = RetentionCheckpoint::Observe;
    sweep.work.after = Some((ended, work_id.clone()));
    maintenance::ensure(pool, KEY, KIND, json!(sweep))
        .await
        .unwrap();

    pass(&state).await.unwrap();

    assert!(
        !exists(
            pool,
            "SELECT EXISTS(SELECT 1 FROM background_work WHERE id=$1)",
            &work_id
        )
        .await
    );
}

#[tokio::test]
async fn another_instance_takes_over_an_expired_claim_and_resumes_its_cursor() {
    let (state, node, cutoff) = fixture().await;
    let pool = &state.pool;
    let ended = cutoff - TimeDelta::hours(2);
    let first_cycle = cycle(pool, "proxy", ended).await;
    let second_cycle = cycle(pool, "proxy", ended + TimeDelta::minutes(1)).await;
    let first = work(pool, &node, first_cycle, "proxy", ended, "complete").await;
    let second = work(
        pool,
        &node,
        second_cycle,
        "proxy",
        ended + TimeDelta::minutes(1),
        "complete",
    )
    .await;
    // The previous owner reached the second page after skipping one locked row.
    let mut sweep = Sweep::new(cutoff);
    sweep.work.after = Some((ended, first.clone()));
    maintenance::ensure(pool, KEY, KIND, json!(sweep))
        .await
        .unwrap();
    let stale = maintenance::claim(pool, KIND, "stale")
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key=$1")
        .bind(&stale.leases[0].resource).execute(pool).await.unwrap();

    pass(&state).await.unwrap();
    assert!(
        exists(
            pool,
            "SELECT EXISTS(SELECT 1 FROM background_work WHERE id=$1)",
            &first
        )
        .await
    );
    assert!(
        !exists(
            pool,
            "SELECT EXISTS(SELECT 1 FROM background_work WHERE id=$1)",
            &second
        )
        .await
    );
    let generation: i64 =
        sqlx::query_scalar("SELECT generation FROM background_work WHERE kind=$1")
            .bind(KIND)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(generation, 2);
    let mut tx = pool.begin().await.unwrap();
    assert!(stale.guard_on(&mut tx).await.is_err());
}

#[tokio::test]
async fn orphan_cycles_remain_for_unsettled_captures_and_pending_observations() {
    let (state, node, cutoff) = fixture().await;
    let pool = &state.pool;
    let ended = cutoff - TimeDelta::hours(2);
    let unsettled = cycle(pool, "traffic", ended).await;
    batch(pool, &node, "orphan-traffic", unsettled, "complete", ended).await;
    sqlx::query("UPDATE traffic_sample_batches SET item_count=1 WHERE sample_id='orphan-traffic'")
        .execute(pool)
        .await
        .unwrap();
    let pending = cycle(pool, "traffic", ended - TimeDelta::minutes(1)).await;
    sqlx::query("INSERT INTO online_samples (cycle_id,node_id,sampled_at,status,connections,cycle_started_at,expected_nodes,pending) VALUES ($1,$2,$3,'failed','{}',$3,1,true)")
        .bind(pending).bind(node).bind(ended).execute(pool).await.unwrap();
    let empty = cycle(pool, "network", ended).await;

    let mut tx = pool.begin().await.unwrap();
    let result = scan_cycles(&mut tx, cutoff, &mut Cursor::default())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(result.cycles_deleted, 1);
    assert_eq!(result.skipped["traffic_batch_unsettled"], 1);
    assert_eq!(result.skipped["online_pending"], 1);
    assert!(
        !exists(
            pool,
            "SELECT EXISTS(SELECT 1 FROM sampling_cycles WHERE id=$1::text::uuid)",
            &empty.to_string()
        )
        .await
    );
}
