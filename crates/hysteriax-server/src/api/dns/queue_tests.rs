use super::transaction_tests::{connection, disable_legacy_lock, fixture, waiting};
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn zone() -> AppState {
    let (state, credential) = fixture().await;
    let (_, Json(connection)) = connection(&state, &credential).await.unwrap();
    sqlx::query("UPDATE dns_connections SET status='verified' WHERE id=$1")
        .bind(connection["id"].as_str().unwrap())
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO dns_zones(id,connection_id,provider_zone_id,name,enabled) VALUES('zone',$1,'remote-zone','example.com',true)").bind(connection["id"].as_str().unwrap()).execute(&state.pool).await.unwrap();
    state
}
fn allocation(key: &str) -> crate::dns::binding::Allocation {
    crate::dns::binding::Allocation {
        idempotency_key: key.into(),
        zone_id: "zone".into(),
        mode: "manual".into(),
        prefix: None,
        hostname: Some("node.example.com".into()),
        ipv4: Some("8.8.8.8".into()),
        ipv6: None,
        record_ids: vec![],
    }
}
async fn node_request(
    state: &AppState,
    allocation: Option<crate::dns::binding::Allocation>,
) -> Value {
    let mut value = json!({"name":"Node","ssh_host":"8.8.8.8","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"test","public_host":"8.8.8.8","public_port":443,"listen_addr":":443"});
    if let Some(allocation) = allocation {
        value["dns_allocation"] = serde_json::to_value(allocation).unwrap();
    }
    let request = crate::credentials::test_node_request(state, value).await;
    serde_json::to_value(request).unwrap()
}
async fn create_node(
    state: &AppState,
    request: Value,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    crate::api::nodes::create(
        State(state.clone()),
        Json(serde_json::from_value(request).unwrap()),
    )
    .await
}
async fn plain_node(state: &AppState) -> String {
    let (_, Json(created)) = create_node(state, node_request(state, None).await)
        .await
        .unwrap();
    created["node"]["id"].as_str().unwrap().into()
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_unique_key_collision_replays_after_rollback_without_rerunning_business_writes() {
    let state = zone().await;
    disable_legacy_lock(&state).await;
    let id: String = sqlx::query_scalar("SELECT id FROM dns_connections")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let calls = Arc::new(AtomicUsize::new(0));
    async fn submit(
        state: &AppState,
        connection: String,
        barrier: Arc<tokio::sync::Barrier>,
        calls: Arc<AtomicUsize>,
    ) -> Result<Value, ApiError> {
        let request = json!({"action":"same"});
        let body = request.clone();
        dns::request_transaction(&state.pool,"native-key",&request,move |tx| {
            let (connection,barrier,calls,body)=(connection.clone(),barrier.clone(),calls.clone(),body.clone());
            Box::pin(async move {
                if let Some(previous)=dns::existing_operation(tx,"native-key",&body).await? {return Ok(previous);}
                barrier.wait().await;
                calls.fetch_add(1, Ordering::SeqCst);
                let id=Uuid::new_v4().to_string();
                sqlx::query("INSERT INTO dns_operations(id,idempotency_key,request_sha256,connection_id,credential_version,resource_type,resource_id,action,payload) VALUES($1,'native-key',$2,$3,1,'dns_connection',$3,'verify','{}')").bind(&id).bind(dns::request_hash(&body)).bind(&connection).execute(&mut **tx).await?;
                Ok(json!({"operation_id":id,"job_id":Value::Null,"resource_id":connection}))
            })
        }).await
    }
    let (a, b) = tokio::join!(
        submit(&state, id.clone(), barrier.clone(), calls.clone()),
        submit(&state, id, barrier, calls.clone())
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        count(&state, "SELECT count(*) FROM dns_operations").await,
        1
    );
}
#[tokio::test]
async fn concurrent_duplicate_record_requests_share_one_record_job_and_operation() {
    let state = zone().await;
    disable_legacy_lock(&state).await;
    let input = CreateRecord {
        zone_id: "zone".into(),
        idempotency_key: "record-key".into(),
        record: RecordInput {
            name: "record.example.com".into(),
            record_type: "A".into(),
            content: "8.8.8.8".into(),
            ttl: 1,
            proxied: false,
        },
    };
    let (a, b) = tokio::join!(
        create_record(State(state.clone()), Json(input.clone())),
        create_record(State(state.clone()), Json(input))
    );
    let (_, Json(a)) = a.unwrap();
    let (_, Json(b)) = b.unwrap();
    assert_eq!(a, b);
    assert_eq!(count(&state, "SELECT count(*) FROM dns_records").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM dns_operations").await,
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
}
#[tokio::test]
async fn competing_requests_for_one_record_name_preserve_one_logical_record() {
    let state = zone().await;
    disable_legacy_lock(&state).await;
    let first = CreateRecord {
        zone_id: "zone".into(),
        idempotency_key: "first".into(),
        record: RecordInput {
            name: "record.example.com".into(),
            record_type: "CNAME".into(),
            content: "target.example.net".into(),
            ttl: 1,
            proxied: false,
        },
    };
    let mut second = first.clone();
    second.idempotency_key = "second".into();
    let (a, b) = tokio::join!(
        create_record(State(state.clone()), Json(first)),
        create_record(State(state.clone()), Json(second))
    );
    let results = [a, b];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| r
                .as_ref()
                .err()
                .is_some_and(|e| e.status == StatusCode::CONFLICT))
            .count(),
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM dns_records").await, 1);
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
}
#[tokio::test]
async fn concurrent_node_creation_with_one_allocation_key_preserves_one_node_and_secret() {
    let state = zone().await;
    let request = node_request(&state, Some(allocation("create-key"))).await;
    disable_legacy_lock(&state).await;
    // Both source transactions reach INSERT with an earlier empty operation
    // snapshot. The loser must roll back its node and recover the winner.
    sqlx::raw_sql("CREATE SEQUENCE node_arrivals; CREATE FUNCTION await_node_peer() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE arrived bigint; deadline timestamptz:=clock_timestamp()+interval '3 seconds'; BEGIN PERFORM nextval('node_arrivals'); LOOP SELECT last_value INTO arrived FROM node_arrivals; EXIT WHEN arrived>=2; IF clock_timestamp()>deadline THEN RAISE EXCEPTION 'node barrier timeout'; END IF; PERFORM pg_sleep(0.01); END LOOP; RETURN NEW; END $$; CREATE TRIGGER await_node_peer BEFORE INSERT ON nodes FOR EACH ROW EXECUTE FUNCTION await_node_peer();").execute(&state.pool).await.unwrap();
    let (a, b) = tokio::join!(
        create_node(&state, request.clone()),
        create_node(&state, request)
    );
    let (sa, Json(a)) = a.unwrap();
    let (sb, Json(b)) = b.unwrap();
    assert!([sa, sb].contains(&StatusCode::CREATED));
    assert!([sa, sb].contains(&StatusCode::OK));
    assert_eq!(a["node"]["id"], b["node"]["id"]);
    let receipt = if sa == StatusCode::CREATED { a } else { b };
    let token = receipt["node_auth_token"].as_str().unwrap();
    let row = sqlx::query("SELECT node_token_hash,node_token_enc FROM nodes")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        row.get::<String, _>("node_token_hash"),
        crate::security::token_digest(token)
    );
    assert_eq!(
        state
            .secrets
            .decrypt(&row.get::<String, _>("node_token_enc"))
            .unwrap(),
        token
    );
    assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM dns_bindings").await, 1);
    assert_eq!(count(&state, "SELECT count(*) FROM dns_records").await, 1);
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='node.created'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn a_binding_waiting_for_zone_disable_rolls_back_its_node_edit() {
    let state = zone().await;
    let node = plain_node(&state).await;
    disable_legacy_lock(&state).await;
    let mut disabling = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM dns_zones WHERE id='zone' FOR UPDATE")
        .fetch_one(&mut *disabling)
        .await
        .unwrap();
    sqlx::query("UPDATE dns_zones SET enabled=false WHERE id='zone'")
        .execute(&mut *disabling)
        .await
        .unwrap();
    let app = state.clone();
    let id = node.clone();
    let work = tokio::spawn(async move {
        set_binding(
            State(app),
            Path(id),
            Json(SetBinding {
                expected_revision: 1,
                allocation: allocation("bind-key"),
            }),
        )
        .await
    });
    waiting(&state, "SELECT id FROM dns_zones WHERE id=$1 FOR SHARE").await;
    disabling.commit().await.unwrap();
    assert_eq!(
        work.await.unwrap().unwrap_err().status,
        StatusCode::BAD_REQUEST
    );
    let (revision, host): (i64, String) =
        sqlx::query_as("SELECT desired_revision,public_host FROM nodes WHERE id=$1")
            .bind(node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!((revision, host), (1, "8.8.8.8".into()));
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM dns_bindings").await, 0);
    assert_eq!(
        count(&state, "SELECT count(*) FROM dns_operations").await,
        0
    );
}
#[tokio::test]
async fn zone_disable_retries_and_sees_a_binding_committed_after_its_snapshot() {
    let state = zone().await;
    let node = plain_node(&state).await;
    disable_legacy_lock(&state).await;
    let allocation = allocation("bind-key");
    let prepared = crate::dns::binding::prepare(&state, &node, &allocation)
        .await
        .unwrap();
    let mut binding = state.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *binding)
        .await
        .unwrap();
    crate::dns::binding::change_hostname_in_tx(&state, &mut binding, &node, 1, &prepared.hostname)
        .await
        .unwrap();
    crate::dns::binding::bind_in_tx(
        &mut binding,
        &node,
        &allocation,
        &prepared,
        &json!({"test":"binding"}),
    )
    .await
    .unwrap();
    let app = state.clone();
    let work = tokio::spawn(async move {
        patch_zone(
            State(app),
            Path("zone".into()),
            Json(ZonePatch {
                expected_revision: 1,
                enabled: false,
            }),
        )
        .await
    });
    waiting(&state, "SELECT id FROM dns_zones WHERE id=$1 FOR UPDATE").await;
    binding.commit().await.unwrap();
    assert_eq!(
        work.await.unwrap().unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT enabled FROM dns_zones WHERE id='zone'")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    assert_eq!(count(&state, "SELECT count(*) FROM dns_bindings").await, 1);
}
#[tokio::test]
async fn retried_node_creation_returns_the_persisted_secret_without_duplicate_history() {
    let state = zone().await;
    let request = node_request(&state, None).await;
    sqlx::raw_sql("CREATE SEQUENCE create_attempt; CREATE FUNCTION retry_node_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='node.created' AND nextval('create_attempt')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_node_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_node_audit();").execute(&state.pool).await.unwrap();
    let (_, Json(receipt)) = create_node(&state, request).await.unwrap();
    let token = receipt["node_auth_token"].as_str().unwrap();
    let (id, cipher, hash): (String, String, String) =
        sqlx::query_as("SELECT id,node_token_enc,node_token_hash FROM nodes")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(id, receipt["node"]["id"].as_str().unwrap());
    assert_eq!(state.secrets.decrypt(&cipher).unwrap(), token);
    assert_eq!(hash, crate::security::token_digest(token));
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='node.created'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM create_attempt").await,
        2
    );
}

#[tokio::test]
async fn native_unique_violation_replays_the_committed_receipt_after_rollback() {
    let state = zone().await;
    let request = json!({"action":"native-collision"});
    let connection: String = sqlx::query_scalar("SELECT id FROM dns_connections")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO dns_operations(id,idempotency_key,request_sha256,connection_id,credential_version,resource_type,resource_id,action,payload) VALUES('winner','native-collision',$1,$2,1,'dns_connection',$2,'verify','{}')").bind(dns::request_hash(&request)).bind(&connection).execute(&state.pool).await.unwrap();
    let mut loser = state.pool.begin().await.unwrap();
    let error=sqlx::query("INSERT INTO dns_operations(id,idempotency_key,request_sha256,connection_id,credential_version,resource_type,resource_id,action,payload) VALUES('loser','native-collision',$1,$2,1,'dns_connection',$2,'verify','{}')").bind(dns::request_hash(&request)).bind(&connection).execute(&mut *loser).await.unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23505")
    );
    loser.rollback().await.unwrap();
    let receipt = dns::replay_after_error(
        &state.pool,
        "native-collision",
        &request,
        ApiError::from(error),
    )
    .await
    .unwrap();
    assert_eq!(
        receipt,
        json!({"operation_id":"winner","job_id":Value::Null,"resource_id":connection})
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM dns_operations").await,
        1
    );
}

#[tokio::test]
async fn concurrent_bindings_of_an_existing_hostname_preserve_one_owner_and_roll_back_the_loser() {
    let state = zone().await;
    let first = plain_node(&state).await;
    let second = plain_node(&state).await;
    sqlx::query("INSERT INTO dns_records(id,zone_id,name,record_type,content,ttl,proxied,origin,state) VALUES('existing','zone','shared.example.com','A','8.8.8.8',1,false,'external','synced')").execute(&state.pool).await.unwrap();
    disable_legacy_lock(&state).await;
    sqlx::raw_sql("CREATE SEQUENCE binding_arrivals; CREATE FUNCTION await_binding_peer() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE arrived bigint; deadline timestamptz:=clock_timestamp()+interval '3 seconds'; BEGIN PERFORM nextval('binding_arrivals'); LOOP SELECT last_value INTO arrived FROM binding_arrivals; EXIT WHEN arrived>=2; IF clock_timestamp()>deadline THEN RAISE EXCEPTION 'binding barrier timeout'; END IF; PERFORM pg_sleep(0.01); END LOOP; RETURN NEW; END $$; CREATE TRIGGER await_binding_peer BEFORE INSERT ON dns_bindings FOR EACH ROW EXECUTE FUNCTION await_binding_peer();").execute(&state.pool).await.unwrap();
    let input = |key: &str| {
        Json(SetBinding {
            expected_revision: 1,
            allocation: crate::dns::binding::Allocation {
                idempotency_key: key.into(),
                zone_id: "zone".into(),
                mode: "existing".into(),
                prefix: None,
                hostname: None,
                ipv4: None,
                ipv6: None,
                record_ids: vec!["existing".into()],
            },
        })
    };
    let (a, b) = tokio::join!(
        set_binding(State(state.clone()), Path(first.clone()), input("first")),
        set_binding(State(state.clone()), Path(second.clone()), input("second"))
    );
    let loser = if a.is_err() { &first } else { &second };
    let results = [a, b];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| r
                .as_ref()
                .err()
                .is_some_and(|e| e.status == StatusCode::CONFLICT))
            .count(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT desired_revision FROM nodes WHERE id=$1")
            .bind(loser)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM dns_bindings").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM dns_operations").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        3
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM dns_records WHERE id='existing' AND origin='external'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn concurrent_duplicate_bindings_advance_the_node_only_once() {
    let state = zone().await;
    let node = plain_node(&state).await;
    disable_legacy_lock(&state).await;
    let input = SetBinding {
        expected_revision: 1,
        allocation: allocation("duplicate-binding"),
    };
    let (a, b) = tokio::join!(
        set_binding(
            State(state.clone()),
            Path(node.clone()),
            Json(input.clone())
        ),
        set_binding(State(state.clone()), Path(node.clone()), Json(input))
    );
    let _ = a.unwrap();
    let _ = b.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT desired_revision FROM nodes WHERE id=$1")
            .bind(node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(count(&state, "SELECT count(*) FROM dns_bindings").await, 1);
    assert_eq!(count(&state, "SELECT count(*) FROM dns_records").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM dns_operations").await,
        2
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='dns.node.bound'"
        )
        .await,
        1
    );
}
