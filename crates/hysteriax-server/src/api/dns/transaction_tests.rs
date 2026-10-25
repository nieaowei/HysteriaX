use super::*;
pub(super) async fn fixture() -> (AppState, String) {
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
    let input=serde_json::from_value(json!({"name":"Cloudflare","kind":"dns","payload":{"provider":"cloudflare","config":{"cloudflare_api_token":"test-token"}}})).unwrap();
    let (_, Json(created)) = super::super::credentials::create(
        State(state.clone()),
        axum::extract::Extension(super::super::AdminActor {
            id: "dns-test".into(),
        }),
        Json(input),
    )
    .await
    .unwrap();
    (state, created["id"].as_str().unwrap().into())
}
pub(super) async fn connection(
    state: &AppState,
    credential: &str,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    create_connection(
        State(state.clone()),
        Json(ConnectionInput {
            name: "Connection".into(),
            credential_id: credential.into(),
            credential_version: 1,
        }),
    )
    .await
}
pub(super) async fn disable_legacy_lock(state: &AppState) {
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
}
pub(super) async fn waiting(state: &AppState, prefix: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE $1 AND wait_event_type='Lock')").bind(format!("{prefix}%")).fetch_one(&state.pool).await.unwrap() {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
}
#[tokio::test]
async fn connection_mutations_and_audits_roll_back_together() {
    let (state, credential) = fixture().await;
    let (_, Json(created)) = connection(&state, &credential).await.unwrap();
    let id = created["id"].as_str().unwrap();
    let snapshot: Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM dns_connections c")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_connection_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action IN ('dns.connection.created','dns.connection.updated','dns.connection.deleted') THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_connection_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_connection_audit();").execute(&state.pool).await.unwrap();
    assert!(connection(&state, &credential).await.is_err());
    assert!(
        patch_connection(
            State(state.clone()),
            Path(id.into()),
            Json(ConnectionPatch {
                expected_revision: 1,
                name: "Changed".into()
            })
        )
        .await
        .is_err()
    );
    assert!(
        delete_connection(
            State(state.clone()),
            Path(id.into()),
            Query(RevisionQuery {
                expected_revision: 1
            })
        )
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, Value>(
            "SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM dns_connections c"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        snapshot
    );
}
#[tokio::test]
async fn concurrent_connection_edits_only_commit_one_revision_without_the_old_locks() {
    let (state, credential) = fixture().await;
    let (_, Json(created)) = connection(&state, &credential).await.unwrap();
    let id = created["id"].as_str().unwrap();
    disable_legacy_lock(&state).await;
    let holder = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    let edit = || {
        Json(ConnectionPatch {
            expected_revision: 1,
            name: "Changed".into(),
        })
    };
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(
            patch_connection(State(state.clone()), Path(id.into()), edit()),
            patch_connection(State(state.clone()), Path(id.into()), edit())
        )
    })
    .await
    .unwrap();
    holder.rollback().await.unwrap();
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
        sqlx::query_scalar::<_, i64>("SELECT revision FROM dns_connections")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
}
#[tokio::test]
async fn a_connection_waiting_for_credential_archival_revalidates_before_insert() {
    let (state, credential) = fixture().await;
    disable_legacy_lock(&state).await;
    let mut archive = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR UPDATE")
        .bind(&credential)
        .fetch_one(&mut *archive)
        .await
        .unwrap();
    sqlx::query("UPDATE credentials SET archived=true WHERE id=$1")
        .bind(&credential)
        .execute(&mut *archive)
        .await
        .unwrap();
    let target = state.clone();
    let id = credential.clone();
    let work = tokio::spawn(async move { connection(&target, &id).await });
    waiting(
        &state,
        "SELECT c.kind,c.owner_user_id,c.archived FROM credentials",
    )
    .await;
    archive.commit().await.unwrap();
    assert_eq!(
        work.await.unwrap().unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM dns_connections")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn connection_deletion_rejects_a_zone_committed_after_its_snapshot() {
    let (state, credential) = fixture().await;
    let (_, Json(created)) = connection(&state, &credential).await.unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    disable_legacy_lock(&state).await;
    let mut zone = state.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO dns_zones(id,connection_id,provider_zone_id,name) VALUES('zone',$1,'remote-zone','example.com')").bind(&id).execute(&mut *zone).await.unwrap();
    let target = state.clone();
    let target_id = id.clone();
    let work = tokio::spawn(async move {
        delete_connection(
            State(target),
            Path(target_id),
            Query(RevisionQuery {
                expected_revision: 1,
            }),
        )
        .await
    });
    waiting(
        &state,
        "SELECT id FROM dns_connections WHERE id=$1 FOR UPDATE",
    )
    .await;
    zone.commit().await.unwrap();
    assert_eq!(
        work.await.unwrap().unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM dns_connections")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM dns_zones")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}
