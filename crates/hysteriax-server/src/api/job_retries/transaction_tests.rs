use super::*;

async fn fixture() -> (AppState, String, String) {
    let pool = db::test_pool_with_max_connections(4).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    crate::credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    let state = AppState::new(pool, secrets);
    let request=crate::credentials::test_node_request(&state,json!({"name":"Node","ssh_host":"8.8.8.8","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"test","public_host":"8.8.8.8","public_port":443,"listen_addr":":443"})).await;
    let (_, Json(receipt)) = super::super::nodes::create(State(state.clone()), Json(request))
        .await
        .unwrap();
    let node = receipt["node"]["id"].as_str().unwrap().to_owned();
    let mut tx = state.pool.begin().await.unwrap();
    let job = super::super::enqueue_job_in_tx(&mut tx, "deploy", Some(&node), Some(1))
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET status='failed',stage='failed',finished_at=now() WHERE id=$1")
        .bind(&job)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    (state, node, job)
}
async fn submit(state: &AppState, job: &str) -> Result<(StatusCode, Json<Value>), ApiError> {
    retry(
        State(state.clone()),
        Path(job.into()),
        Json(RetryRequest {
            expected_revision: 1,
        }),
    )
    .await
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
#[tokio::test]
async fn concurrent_retries_share_one_child_and_audit_without_legacy_lock() {
    let (state, _, job) = fixture().await;
    let legacy = db::begin_write(&state.pool).await.unwrap();
    let (a, b) = tokio::join!(submit(&state, &job), submit(&state, &job));
    legacy.rollback().await.unwrap();
    let (_, Json(a)) = a.unwrap();
    let (_, Json(b)) = b.unwrap();
    assert_eq!(a, b);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE retry_of_job_id IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='job.retry_requested'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn audit_failure_rolls_back_child_event_and_parent_timestamp() {
    let (state, _, job) = fixture().await;
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(j) FROM jobs j WHERE id=$1")
        .bind(&job)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let events = count(&state, "SELECT count(*) FROM job_events").await;
    sqlx::raw_sql("CREATE FUNCTION reject_retry_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='job.retry_requested' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_retry_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_retry_audit();").execute(&state.pool).await.unwrap();
    assert!(submit(&state, &job).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(j) FROM jobs j WHERE id=$1")
            .bind(&job)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM job_events").await,
        events
    );
}
#[tokio::test]
async fn native_retry_conflict_preserves_one_child_and_actual_actor() {
    let (state, _, job) = fixture().await;
    sqlx::raw_sql("CREATE SEQUENCE retry_attempts; CREATE FUNCTION retry_retry_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='job.retry_requested' AND nextval('retry_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_retry_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_retry_audit();").execute(&state.pool).await.unwrap();
    let (_, Json(receipt)) = super::super::AUDIT_ACTOR_ID
        .scope("retry-operator".into(), submit(&state, &job))
        .await
        .unwrap();
    let child: String = sqlx::query_scalar("SELECT id FROM jobs WHERE retry_of_job_id=$1")
        .bind(job)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(child, receipt["job_id"].as_str().unwrap());
    assert_eq!(count(&state,"SELECT count(*) FROM audit_records WHERE action='job.retry_requested' AND actor='retry-operator'").await,1);
    assert_eq!(
        count(&state, "SELECT last_value FROM retry_attempts").await,
        2
    );
}
#[tokio::test]
async fn blocked_remote_outcomes_cannot_be_retried_without_verification() {
    let (state, node, job) = fixture().await;
    sqlx::query("INSERT INTO resource_leases(resource_key,work_id,blocked) VALUES($1,$2,true)")
        .bind(format!("node:{node}"))
        .bind(&job)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        submit(&state, &job).await.unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM resource_leases WHERE blocked").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='job.retry_requested'"
        )
        .await,
        0
    );
}
#[tokio::test]
async fn retry_waiting_for_node_change_rechecks_revision() {
    let (state, node, job) = fixture().await;
    let mut edit = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE nodes SET desired_revision=2 WHERE id=$1")
        .bind(&node)
        .execute(&mut *edit)
        .await
        .unwrap();
    let (app, id) = (state.clone(), job.clone());
    let request = tokio::spawn(async move { submit(&app, &id).await });
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE%' AND wait_event_type='Lock')").fetch_one(&state.pool).await.unwrap() {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    edit.commit().await.unwrap();
    assert_eq!(
        request.await.unwrap().unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
}

async fn dns_fixture() -> (AppState, String) {
    let (state, _, _) = fixture().await;
    let mut tx = state.pool.begin().await.unwrap();
    let credential = crate::credentials::insert(
        &mut tx,
        &state.secrets,
        "DNS",
        "dns",
        None,
        &json!({"provider":"cloudflare","config":{"cloudflare_api_token":"test"}}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO dns_connections(id,name,provider,credential_id,credential_version,status) VALUES('connection','DNS','cloudflare',$1,1,'verified')").bind(credential).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO dns_zones(id,connection_id,provider_zone_id,name,enabled) VALUES('zone','connection','remote-zone','example.com',true)").execute(&mut *tx).await.unwrap();
    let job = super::super::enqueue_job_with_payload_in_tx(
        &mut tx,
        "dns-zone-refresh",
        None,
        None,
        json!({"dns_operation_id":"operation"}),
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO dns_operations(id,idempotency_key,request_sha256,connection_id,credential_version,resource_type,resource_id,action,payload,job_id) VALUES('operation','operation','digest','connection',1,'dns_zone','zone','zone-refresh','{}',$1)").bind(&job).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE jobs SET status='failed',stage='failed',finished_at=now(),resource_key='dns-zone:zone' WHERE id=$1").bind(&job).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    (state, job)
}
#[tokio::test]
async fn concurrent_dns_retries_share_the_operation_successor_and_audit() {
    let (state, job) = dns_fixture().await;
    let (a, b) = tokio::join!(submit(&state, &job), submit(&state, &job));
    let (_, Json(a)) = a.unwrap();
    let (_, Json(b)) = b.unwrap();
    assert_eq!(a, b);
    let active: String =
        sqlx::query_scalar("SELECT job_id FROM dns_operations WHERE id='operation'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(active, a["job_id"].as_str().unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE retry_of_job_id IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='job.retry_requested'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn dns_retry_audit_failure_preserves_the_original_operation_job() {
    let (state, job) = dns_fixture().await;
    sqlx::raw_sql("CREATE FUNCTION reject_dns_retry_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='job.retry_requested' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_dns_retry_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_dns_retry_audit();").execute(&state.pool).await.unwrap();
    assert!(submit(&state, &job).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT job_id FROM dns_operations WHERE id='operation'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        job
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE retry_of_job_id IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='job.retry_requested'"
        )
        .await,
        0
    );
}
