use super::*;
use crate::security::SecretBox;
use axum::{Json, extract::State};
async fn node(state: &AppState) -> String {
    let request=crate::credentials::test_node_request(state,json!({"name":"Sample node","ssh_host":"127.0.0.1","ssh_port":1,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"test","public_host":"example.test","public_port":443,"listen_addr":":443"})).await;
    let (_, Json(receipt)) = crate::api::nodes::create(State(state.clone()), Json(request))
        .await
        .unwrap();
    let id = receipt["node"]["id"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    id
}
async fn fixture() -> (AppState, String) {
    let pool = db::test_pool_with_max_connections(6).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    crate::credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    let state = AppState::new(pool, secrets);
    let id = node(&state).await;
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    (state, id)
}
async fn freeze(state: &AppState, kind: &str) -> Uuid {
    let id = ensure_cycle(&state.pool, kind).await.unwrap();
    // Ownership tests control deadlines separately from the wall-clock slot.
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour' WHERE cycle_id=$1").bind(id).execute(&state.pool).await.unwrap();
    id
}
async fn expire_leases(state: &AppState) {
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
#[tokio::test]
async fn shared_periods_freeze_membership_and_online_denominators() {
    let (state, first) = fixture().await;
    let (a, b) = tokio::join!(
        ensure_cycle(&state.pool, "traffic"),
        ensure_cycle(&state.pool, "traffic")
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    let rows: Vec<(Uuid, DateTime<Utc>, i64)> = sqlx::query_as(
        "SELECT id,period_start,expected_nodes FROM sampling_cycles ORDER BY period_start",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    for (id, _, expected) in &rows {
        assert_eq!(*expected, 1);
        let nodes: Vec<String> =
            sqlx::query_scalar("SELECT node_id FROM background_work WHERE cycle_id=$1")
                .bind(id)
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(nodes, vec![first.clone()]);
    }
    if rows.len() == 1 {
        assert_eq!(a, b);
    } else {
        assert_ne!(rows[0].1, rows[1].1);
    }
    let _second = node(&state).await;
    let _ = ensure_cycle(&state.pool, "traffic").await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT expected_nodes FROM sampling_cycles WHERE id=$1")
            .bind(a)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM background_work WHERE cycle_id=$1")
            .bind(a)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT expected_nodes FROM online_samples WHERE cycle_id=$1")
            .bind(a)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn claims_share_node_leases_with_jobs_and_skip_unrelated_busy_nodes() {
    let (state, first) = fixture().await;
    let second = node(&state).await;
    freeze(&state, "traffic").await;
    let mut tx = state.pool.begin().await.unwrap();
    let job = execution::acquire(&mut tx, &format!("node:{first}"), "job-owner", "job-id")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let legacy = db::begin_write(&state.pool).await.unwrap();
    let work = claim(&state.pool, "traffic", "one").await.unwrap().unwrap();
    assert_eq!(work.node, second);
    assert!(
        claim(&state.pool, "traffic", "two")
            .await
            .unwrap()
            .is_none()
    );
    legacy.rollback().await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    assert!(job.release(&mut tx).await.unwrap());
    tx.commit().await.unwrap();
    let other = claim(&state.pool, "traffic", "two").await.unwrap().unwrap();
    assert_eq!(other.node, first);
}
#[tokio::test]
async fn takeover_preserves_capture_identity_and_fences_previous_owner() {
    let (state, _) = fixture().await;
    freeze(&state, "network").await;
    let old = claim(&state.pool, "network", "old").await.unwrap().unwrap();
    expire_leases(&state).await;
    let next = claim(&state.pool, "network", "next")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.id, next.id);
    assert_eq!(old.payload, next.payload);
    assert!(
        next.leases
            .iter()
            .find(|l| l.resource.starts_with("work:"))
            .unwrap()
            .generation
            > old
                .leases
                .iter()
                .find(|l| l.resource.starts_with("work:"))
                .unwrap()
                .generation
    );
    assert!(finish(&state.pool, &old, true).await.is_err());
    let result = execution::supervise(state.pool.clone(), old.leases.clone(), async {
        panic!("a stale owner must not start another step");
    })
    .await;
    assert!(result.is_none());
    execution::supervise(
        state.pool.clone(),
        next.leases.clone(),
        finish(&state.pool, &next, true),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE state='complete'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn overdue_uncaptured_work_expires_without_resetting_a_healthy_owner() {
    let (state, _) = fixture().await;
    freeze(&state, "traffic").await;
    let work = claim(&state.pool, "traffic", "healthy")
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(expire(&state.pool).await.unwrap(), 0);
    assert!(
        claim(&state.pool, "traffic", "other")
            .await
            .unwrap()
            .is_none()
    );
    expire_leases(&state).await;
    assert_eq!(expire(&state.pool).await.unwrap(), 1);
    assert!(finish(&state.pool, &work, true).await.is_err());
    assert_eq!(count(&state,"SELECT count(*) FROM online_samples WHERE NOT pending AND status='failed' AND traffic_status='failed'").await,1);
    assert!(
        claim(&state.pool, "traffic", "other")
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn captured_traffic_survives_slot_expiry_and_recovers_without_ssh_or_double_billing() {
    let (state, node) = fixture().await;
    freeze(&state, "traffic").await;
    let old = claim(&state.pool, "traffic", "old").await.unwrap().unwrap();
    let user = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES($1,'Billing',true,now(),now())").bind(&user).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES($1,$2,'billing',$3,clock_timestamp())").bind(&user).bind(&node).bind(state.secrets.encrypt("billing-credential").unwrap()).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO traffic_baselines(node_id,user_id,instance_id,tx_total,rx_total,sampled_at,sample_order) VALUES($1,$2,'service-instance',0,0,clock_timestamp()-interval '1 hour',0)").bind(&node).bind(&user).execute(&state.pool).await.unwrap();
    execution::supervise(state.pool.clone(), old.leases.clone(), async {
        crate::traffic::batches::reserve(&state.pool, &old.id, &node, Some(old.cycle))
            .await
            .unwrap();
        let counters = serde_json::from_value(json!({user.clone():{"tx":100,"rx":200}})).unwrap();
        crate::traffic::batches::seal(&state.pool, &old.id, "service-instance", &counters)
            .await
            .unwrap();
    })
    .await
    .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    expire_leases(&state).await;
    assert_eq!(expire(&state.pool).await.unwrap(), 0);
    let next = claim(&state.pool, "traffic", "next")
        .await
        .unwrap()
        .unwrap();
    execution::supervise(state.pool.clone(), next.leases.clone(), async {
        crate::traffic::sample_work(&state, &next).await.unwrap();
        finish(&state.pool, &next, true).await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id=$1")
            .bind(user)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        300
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM traffic_records").await,
        1
    );
    assert!(
        claim(&state.pool, "traffic", "third")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
}
#[tokio::test]
async fn deferred_billing_requeues_the_existing_capture_and_releases_its_leases() {
    let (state, node) = fixture().await;
    freeze(&state, "traffic").await;
    let work = claim(&state.pool, "traffic", "one").await.unwrap().unwrap();
    execution::supervise(state.pool.clone(), work.leases.clone(), async {
        crate::traffic::batches::reserve(&state.pool, &work.id, &node, Some(work.cycle))
            .await
            .unwrap();
        crate::traffic::batches::seal(&state.pool, &work.id, "instance", &serde_json::Map::new())
            .await
            .unwrap();
        finish(&state.pool, &work, false).await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE state='queued' AND error_code='billing_deferred'").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL"
        )
        .await,
        0
    );
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(
        claim(&state.pool, "traffic", "two")
            .await
            .unwrap()
            .is_some()
    );
}
#[tokio::test]
async fn deletion_invalidates_even_a_live_sampling_write_context() {
    let (state, node) = fixture().await;
    freeze(&state, "network").await;
    let work = claim(&state.pool, "network", "owner")
        .await
        .unwrap()
        .unwrap();
    let result = execution::supervise(state.pool.clone(), work.leases.clone(), async {
        sqlx::query("UPDATE nodes SET state='deleting' WHERE id=$1")
            .bind(node)
            .execute(&state.pool)
            .await
            .unwrap();
        db::write::<_, sqlx::Error, _>(&state.pool, |tx| {
            Box::pin(async move {
                sqlx::query("UPDATE background_work SET error_code='stale'")
                    .execute(&mut **tx)
                    .await?;
                Ok(())
            })
        })
        .await
        .is_err()
    })
    .await;
    assert!(result.is_none_or(|rejected| rejected));
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE error_code='stale'"
        )
        .await,
        0
    );
}
#[tokio::test]
async fn expiry_is_bounded_and_skips_work_rows_claimed_by_another_transaction() {
    let (state, node) = fixture().await;
    let cycle = freeze(&state, "network").await;
    sqlx::query("INSERT INTO background_work(id,kind,cycle_id,node_id,deadline_at) SELECT 'expired-'||i,'network',$1,$2||i,clock_timestamp()-interval '1 hour' FROM generate_series(1,301) i").bind(cycle).bind(node).execute(&state.pool).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM background_work WHERE id='expired-1' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    assert_eq!(expire(&state.pool).await.unwrap(), 256);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE state='expired'"
        )
        .await,
        256
    );
    held.rollback().await.unwrap();
    assert_eq!(expire(&state.pool).await.unwrap(), 45);
}

#[tokio::test]
async fn recorded_network_results_are_acknowledged_without_another_ssh_read() {
    for failure in [false, true] {
        let (state, node) = fixture().await;
        freeze(&state, "network").await;
        let work = claim(&state.pool, "network", "first")
            .await
            .unwrap()
            .unwrap();
        let captured: DateTime<Utc> =
            serde_json::from_value(work.payload["captured_at"].clone()).unwrap();
        sqlx::query("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,gap_reason,sampled_at) SELECT node_id,period_id,0,0,$2,$3 FROM node_packages WHERE node_id=$1").bind(&node).bind(if failure{Some("network_sample_failed")}else{None}).bind(captured).execute(&state.pool).await.unwrap();
        expire_leases(&state).await;
        let next = claim(&state.pool, "network", "next")
            .await
            .unwrap()
            .unwrap();
        execution::supervise(state.pool.clone(), next.leases.clone(), async {
            let result = crate::node_limits::sample_work(&state, &next).await;
            assert_eq!(result.is_err(), failure);
            if failure {
                assert!(result.unwrap_err().to_string().contains("already recorded"));
            }
            finish(&state.pool, &next, !failure).await.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(
            count(&state, "SELECT count(*) FROM node_network_samples").await,
            1
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM execution_operations").await,
            0
        );
    }
}

#[tokio::test]
async fn sampling_migration_preserves_config_ciphertext_usage_and_old_online_history() {
    let pool = db::test_pool_through_version(4, 49).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    crate::credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    let state = AppState::new(pool, secrets);
    let node = node(&state).await;
    sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,status,connections,cycle_started_at,expected_nodes,traffic_status) VALUES($1,$2,'2026-01-01T00:00:00Z','ok','{}','2026-01-01T00:00:00Z',1,'ok')").bind(Uuid::new_v4()).bind(node).execute(&state.pool).await.unwrap();
    let query = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'configs',(SELECT jsonb_agg(to_jsonb(v) ORDER BY id) FROM config_versions v),'credentials',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v),'packages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY node_id) FROM node_packages p),'online',(SELECT jsonb_agg(to_jsonb(s)-'pending' ORDER BY id) FROM online_samples s))";
    let before: Value = sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    db::run_migrations(&state.pool).await.unwrap();
    db::run_migrations(&state.pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Value>(query)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM online_samples WHERE pending").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM background_work").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM sampling_cycles").await,
        0
    );
}

#[tokio::test]
async fn a_claimant_paused_past_the_slot_cannot_start_an_uncaptured_read() {
    for kind in ["traffic", "network", "proxy"] {
        let (state, _) = fixture().await;
        freeze(&state, kind).await;
        let work = claim(&state.pool, kind, "paused").await.unwrap().unwrap();
        sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()-interval '1 second'")
            .execute(&state.pool)
            .await
            .unwrap();
        execution::supervise(state.pool.clone(), work.leases.clone(), async {
            let error = if kind == "traffic" {
                crate::traffic::sample_work(&state, &work)
                    .await
                    .unwrap_err()
            } else if kind == "proxy" {
                crate::proxy_monitor::sample_work(&state, &work)
                    .await
                    .unwrap_err()
            } else {
                crate::node_limits::sample_work(&state, &work)
                    .await
                    .unwrap_err()
            };
            assert!(
                error
                    .to_string()
                    .contains("slot expired before remote collection")
            );
        })
        .await
        .unwrap();
        assert_eq!(
            count(&state, "SELECT count(*) FROM traffic_sample_batches").await,
            0
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM node_network_samples").await,
            0
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM execution_operations").await,
            0
        );
    }
}
