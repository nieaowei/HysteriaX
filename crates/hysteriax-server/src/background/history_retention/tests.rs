use super::*;
use crate::security::SecretBox;
use axum::{Json, extract::State};
use serde_json::json;
use sqlx::PgPool;

async fn fixture() -> (AppState, String) {
    let pool = db::test_pool_with_max_connections(8).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    let state = AppState::new(pool, secrets);
    let request = crate::credentials::test_node_request(&state, json!({
        "name":"History retention node","ssh_host":"127.0.0.1","ssh_port":1,"ssh_username":"root",
        "ssh_auth_type":"password","ssh_secret":"test","public_host":"example.test","public_port":443,
        "listen_addr":":443"
    })).await;
    let (_, Json(receipt)) = crate::api::nodes::create(State(state.clone()), Json(request))
        .await
        .unwrap();
    (state, receipt["node"]["id"].as_str().unwrap().to_owned())
}

async fn run_page(pool: &PgPool, task: Task, now: DateTime<Utc>) -> Counts {
    let mut tx = pool.begin().await.unwrap();
    let counts = task
        .page(&mut tx, &mut Cursor::default(), now, None, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    counts
}

#[tokio::test]
async fn history_retention_prunes_only_settled_details_and_keeps_a_restart_checkpoint() {
    let (state, node) = fixture().await;
    let db = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(65);
    for (id, status, at, processed) in [
        ("old", "complete", old, 1_i64),
        ("checkpoint", "complete", old, 1),
        ("unsettled", "ready", old, 0),
        ("recent", "complete", now - TimeDelta::days(10), 1),
    ] {
        sqlx::query(
            "INSERT INTO traffic_sample_batches
             (sample_id,node_id,instance_id,state,payload_digest,sampled_at,completed_at,item_count,processed_count)
             VALUES($1,$2,CASE WHEN $1='recent' THEN NULL ELSE 'instance' END,$3,'digest',$4,
                    CASE WHEN $3='complete' THEN $4 END,1,$5)",
        ).bind(id).bind(&node).bind(status).bind(at).bind(processed).execute(db).await.unwrap();
        sqlx::query(
            "INSERT INTO traffic_sample_items(sample_id,node_id,user_id,tx_total,rx_total,sampled_at,processed_at)
             VALUES($1,$2,'user',123,321,$3,CASE WHEN $4>0 THEN $3 END)",
        ).bind(id).bind(&node).bind(at).bind(processed).execute(db).await.unwrap();
    }
    let deleted = run_page(db, Task::Rows(rows::ITEMS), now).await;
    assert_eq!((deleted.eligible, deleted.deleted), (2, 2));
    let batches = run_page(db, Task::Rows(rows::BATCHES), now).await;
    assert_eq!(batches.deleted, 1);
    assert_eq!(batches.skipped.get("latest_instance"), Some(&1));
    let remaining: Vec<String> =
        sqlx::query_scalar("SELECT sample_id FROM traffic_sample_batches ORDER BY sample_id")
            .fetch_all(db)
            .await
            .unwrap();
    assert_eq!(remaining, ["checkpoint", "recent", "unsettled"]);
}

#[tokio::test]
async fn history_retention_preserves_usage_and_the_current_monthly_period() {
    let (state, node) = fixture().await;
    let db = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(65);
    sqlx::query(
        "UPDATE node_packages SET config=jsonb_set(config,'{cycle}','\"monthly\"'),period_id='current',usage_bytes=99
         WHERE node_id=$1",
    ).bind(&node).execute(db).await.unwrap();
    sqlx::query(
        "INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,sampled_at)
         VALUES ($1,'current',11,12,$2),($1,'ended',21,22,$2),($1,'ended',31,32,$3)",
    )
    .bind(&node)
    .bind(old)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
    let counts = run_page(db, Task::Rows(rows::NETWORK), now).await;
    assert_eq!(counts.deleted, 1);
    assert_eq!(counts.skipped.get("current_monthly_period"), Some(&1));
    sqlx::query(
        "UPDATE node_packages SET config=jsonb_set(config,'{cycle}','\"fixed\"') WHERE node_id=$1",
    )
    .bind(&node)
    .execute(db)
    .await
    .unwrap();
    assert_eq!(
        run_page(db, Task::Rows(rows::NETWORK), now).await.deleted,
        1
    );
    crate::analytics::refresh(db).await.unwrap();
    let (usage, total): (i64, i64) = sqlx::query_as(
        "SELECT usage_bytes,(SELECT sum(tx)::bigint FROM traffic_hour_totals WHERE source='network')
         FROM node_packages WHERE node_id=$1",
    ).bind(&node).fetch_one(db).await.unwrap();
    assert_eq!((usage, total), (99, 31));
}

#[tokio::test]
async fn history_retention_prunes_a_publication_prefix_and_only_unreferenced_job_chains() {
    let db = db::test_pool().await;
    let now = Utc::now();
    let old = now - TimeDelta::days(100);
    for (id, parent) in [
        ("root", None),
        ("child", Some("root")),
        ("referenced", None),
    ] {
        sqlx::query(
            "INSERT INTO jobs(id,kind,status,stage,created_at,updated_at,available_at,finished_at,retry_of_job_id)
             VALUES($1,'fixture','succeeded','done',$2,$2,$2,$2,$3)",
        ).bind(id).bind(old).bind(parent).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES($1,'job.succeeded','{}',$2)")
            .bind(id).bind(old).execute(&db).await.unwrap();
    }
    assert_eq!(crate::events::publish_pending(&db).await.unwrap(), 3);
    sqlx::query("UPDATE event_publications SET published_at=$1 WHERE publication_seq IN (1,2)")
        .bind(old)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(run_page(&db, Task::Publications, now).await.deleted, 2);
    assert_eq!(
        run_page(&db, Task::Rows(rows::JOB_EVENTS), now)
            .await
            .deleted,
        2
    );
    let mut tx = db.begin().await.unwrap();
    let mut cursor = Cursor::default();
    let mut jobs = 0;
    while !cursor.done {
        jobs += Task::JobChains
            .page(&mut tx, &mut cursor, now, None, None)
            .await
            .unwrap()
            .deleted;
    }
    tx.commit().await.unwrap();
    assert_eq!(jobs, 2);
    let surviving: Vec<String> = sqlx::query_scalar("SELECT id FROM jobs ORDER BY id")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(surviving, ["referenced"]);
    let positions: Vec<i64> = sqlx::query_scalar(
        "SELECT publication_seq FROM event_publications ORDER BY publication_seq",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(positions, [3]);
}

#[tokio::test]
async fn recycled_dirty_keys_cannot_be_acknowledged_by_old_refreshes() {
    let db = db::test_pool_with_max_connections(2).await;
    let key = json!({"job_id":"reused"});
    let mark = "SELECT hysteriax_mark_dirty('job_retry_state_dirty',$1)";
    sqlx::query(mark).bind(&key).execute(&db).await.unwrap();
    let stale: i64 = sqlx::query_scalar(
        "DELETE FROM job_retry_state_dirty WHERE job_id='reused' RETURNING generation",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query(mark).bind(&key).execute(&db).await.unwrap();
    let attempted =
        sqlx::query("DELETE FROM job_retry_state_dirty WHERE job_id='reused' AND generation=$1")
            .bind(stale)
            .execute(&db)
            .await
            .unwrap();
    assert_eq!(attempted.rows_affected(), 0);
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT to_regclass('dirty_key_versions') IS NOT NULL")
            .fetch_one(&db)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn history_retention_restarts_legacy_observation_and_updates_audit_totals() {
    let pool = db::test_pool_with_max_connections(4).await;
    let state = AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    sqlx::query(
        "INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at)
         VALUES('old','actor','test','entity','1','{}',clock_timestamp()-interval '91 days'),
               ('new','actor','test','entity','2','{}',clock_timestamp())",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let group = GROUPS[2];
    let now = Utc::now();
    let mut cursors: Vec<_> = group.tasks.iter().map(|_| Cursor::default()).collect();
    cursors[5].after = Some((now - TimeDelta::days(90), "old".into()));
    maintenance::ensure(
        &state.pool,
        group.key,
        group.key,
        json!(Sweep {
            checkpoint: RetentionCheckpoint::Observe,
            now,
            next: 5,
            cursors,
            busy: false,
        }),
    )
    .await
    .unwrap();
    pass(&state, group).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT total FROM audit_totals WHERE id")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    let remaining: String =
        sqlx::query_scalar("SELECT id FROM audit_records WHERE id IN ('old','new')")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(remaining, "new");
}

#[tokio::test]
async fn history_retention_preserves_user_quota_and_unfinished_billing() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await
        .unwrap();
    let old = now - TimeDelta::days(61);
    let boundary = now - TimeDelta::days(60);
    sqlx::query(
        "INSERT INTO users(id,name,created_at,updated_at,usage_bytes)
         VALUES('billed','Billed',$1,$1,100)",
    )
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO traffic_sample_batches(sample_id,node_id,state,payload_digest,sampled_at,item_count)
         VALUES('pending',$1,'ready','digest',$2,1)",
    ).bind(&node).bind(old).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO traffic_records
         (id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at,sample_id)
         VALUES ('expired',$1,'billed','i',10,0,10,0,$2,NULL),
                ('in_flight',$1,'billed','i',20,0,20,0,$2,'pending'),
                ('boundary',$1,'billed','i',30,0,30,0,$3,NULL)",
    ).bind(&node).bind(old).bind(boundary).execute(pool).await.unwrap();
    let counts = run_page(pool, Task::Rows(rows::RECORDS), now).await;
    assert_eq!(counts.deleted, 1);
    assert_eq!(counts.skipped.get("batch_unsettled"), Some(&1));
    let totals: (i64, i64, i64) = sqlx::query_as(
        "SELECT u.usage_bytes,s.tx::bigint,s.samples
         FROM users u JOIN user_node_usage s ON s.user_id=u.id WHERE u.id='billed'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(totals, (100, 50, 2));
}

#[tokio::test]
async fn history_retention_skips_locked_rows_and_retries_after_rollback() {
    let pool = db::test_pool_with_max_connections(4).await;
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at)
         VALUES('locked','a','test','entity','1','{}',$1),('other','a','test','entity','2','{}',$1)",
    ).bind(now - TimeDelta::days(91)).execute(&pool).await.unwrap();
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM audit_records WHERE id='locked' FOR UPDATE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let mut attempt = pool.begin().await.unwrap();
    let counts = Task::Rows(rows::AUDIT)
        .page(&mut attempt, &mut Cursor::default(), now, None, None)
        .await
        .unwrap();
    assert_eq!(counts.deleted, 1);
    assert_eq!(counts.skipped.get("row_or_parent_locked_or_gone"), Some(&1));
    attempt.rollback().await.unwrap();
    lock.rollback().await.unwrap();
    assert_eq!(
        run_page(&pool, Task::Rows(rows::AUDIT), now).await.deleted,
        2
    );
}

#[tokio::test]
async fn history_retention_yields_to_nodes_with_live_execution_leases() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO node_network_samples (node_id,period_id,delta_tx,delta_rx,sampled_at)
         VALUES($1,'past',1,1,$2)",
    )
    .bind(&node)
    .bind(now - TimeDelta::days(65))
    .execute(pool)
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let lease = execution::acquire(&mut tx, &format!("node:{node}"), "sampler", "work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let skipped = run_page(pool, Task::Rows(rows::NETWORK), now).await;
    assert_eq!(skipped.deleted, 0);
    assert_eq!(
        skipped.skipped.get("row_or_parent_locked_or_gone"),
        Some(&1)
    );

    let mut tx = pool.begin().await.unwrap();
    assert!(lease.release(&mut tx).await.unwrap());
    tx.commit().await.unwrap();
    assert_eq!(
        run_page(pool, Task::Rows(rows::NETWORK), now).await.deleted,
        1
    );
}

#[tokio::test]
async fn history_retention_pages_equal_timestamps_in_native_numeric_order() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let expired = now - TimeDelta::days(65);
    sqlx::query(
        "INSERT INTO node_network_samples (node_id,period_id,delta_tx,delta_rx,sampled_at)
         SELECT $1,'past',1,1,$2 FROM generate_series(1,1200)",
    )
    .bind(&node)
    .bind(expired)
    .execute(pool)
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let mut cursor = Cursor::default();
    let mut page_sizes = Vec::new();
    for _ in 0..2 {
        let counts = Task::Rows(rows::NETWORK)
            .page(&mut tx, &mut cursor, now, None, None)
            .await
            .unwrap();
        assert_eq!(counts.deleted, counts.eligible as u64);
        page_sizes.push(counts.eligible);
    }
    assert!(cursor.done);
    assert_eq!(page_sizes, [1024, 176]);
    tx.commit().await.unwrap();
}

#[test]
fn history_retention_preserves_headroom_for_both_sampling_intervals() {
    let wait = |millis, interval| sampling_headroom_at(millis, interval).as_millis();
    assert_eq!(wait(6_000, 60), 6_000);
    assert_eq!(wait(12_000, 60), 0);
    assert_eq!(wait(59_500, 60), 12_500);
    assert_eq!(wait(20_500, 10), 1_500);
    assert_eq!(wait(25_000, 10), 0);
    assert_eq!(wait(31_000, 30), 5_000);
}

#[tokio::test]
async fn ledger_page_retires_newly_unreferenced_batches_without_losing_the_checkpoint() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let past = now - TimeDelta::days(65);
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('billed','Billed',$1,$1)")
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    for (id, at) in [
        ("can_retire", past),
        ("protected", past),
        ("checkpoint", now),
    ] {
        sqlx::query(
            "INSERT INTO traffic_sample_batches
             (sample_id,node_id,instance_id,state,payload_digest,sampled_at,completed_at,item_count,processed_count)
             VALUES($1,$2,'instance','complete','digest',$3,$3,1,1)",
        ).bind(id).bind(&node).bind(at).execute(pool).await.unwrap();
        sqlx::query(
            "INSERT INTO traffic_records
             (id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at,sample_id)
             VALUES($1,$2,'billed','instance',0,0,1,1,$3,$1)",
        ).bind(id).bind(&node).bind(at).execute(pool).await.unwrap();
    }
    sqlx::query(
        "INSERT INTO traffic_sample_items(sample_id,node_id,user_id,tx_total,rx_total,sampled_at,processed_at)
         VALUES('protected',$1,'billed',1,1,$2,$2)",
    )
    .bind(&node)
    .bind(past)
    .execute(pool)
    .await
    .unwrap();
    for commit in [false, true] {
        let mut tx = pool.begin().await.unwrap();
        let counts = Task::Rows(rows::RECORDS)
            .page(
                &mut tx,
                &mut Cursor::default(),
                now,
                None,
                Some(now - TimeDelta::days(60)),
            )
            .await
            .unwrap();
        assert_eq!(counts.deleted, 2);
        assert_eq!(counts.locked_batches, ["can_retire", "protected"]);
        let retired = rows::checkpoint(
            &mut tx,
            "fixture",
            &json!(null),
            now - TimeDelta::days(60),
            &counts.locked_batches,
        )
        .await
        .unwrap();
        assert_eq!(retired, 1);
        if commit {
            tx.commit().await.unwrap();
        } else {
            tx.rollback().await.unwrap();
        }
    }

    let remaining: Vec<String> =
        sqlx::query_scalar("SELECT sample_id FROM traffic_sample_batches ORDER BY sample_id")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(remaining, ["checkpoint", "protected"]);

    let mut tx = pool.begin().await.unwrap();
    let cutoff = now - TimeDelta::days(60);
    let counts = Task::Rows(rows::ITEMS)
        .page(&mut tx, &mut Cursor::default(), now, None, Some(cutoff))
        .await
        .unwrap();
    assert_eq!(counts.deleted, 1);
    assert_eq!(counts.locked_batches, ["protected"]);
    assert_eq!(
        rows::checkpoint(
            &mut tx,
            "fixture",
            &json!(null),
            cutoff,
            &counts.locked_batches
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    let remaining: Vec<String> =
        sqlx::query_scalar("SELECT sample_id FROM traffic_sample_batches ORDER BY sample_id")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(remaining, ["checkpoint"]);
}
