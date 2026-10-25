use super::*;
async fn fixture() -> AppState {
    let state = super::tests::fixture().await;
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    state
}
async fn preview(state: &AppState) -> Result<Json<Value>, ApiError> {
    preview_create(
        State(state.clone()),
        Json(
            serde_json::from_value(
                json!({"action":"create","name":"Group","user_ids":["user"],"node_ids":["node"]}),
            )
            .unwrap(),
        ),
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
async fn concurrent_previews_are_independent_and_do_not_change_authorization() {
    let state = fixture().await;
    let assignments = count(&state, "SELECT count(*) FROM node_assignments").await;
    let legacy = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(preview(&state), preview(&state));
    let Json(a) = a.unwrap();
    let Json(b) = b.unwrap();
    legacy.rollback().await.unwrap();
    assert_ne!(a["preview_token"], b["preview_token"]);
    assert_eq!(a["additions"], b["additions"]);
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_group_previews").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_groups").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        assignments
    );
}
#[tokio::test]
async fn retried_preview_returns_only_the_one_committed_token() {
    let state = fixture().await;
    sqlx::raw_sql("CREATE SEQUENCE preview_attempts; CREATE FUNCTION retry_preview_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('preview_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_preview_insert AFTER INSERT ON authorization_group_previews FOR EACH ROW EXECUTE FUNCTION retry_preview_insert();").execute(&state.pool).await.unwrap();
    let Json(receipt) = preview(&state).await.unwrap();
    let token: String = sqlx::query_scalar("SELECT token FROM authorization_group_previews")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(token, receipt["preview_token"].as_str().unwrap());
    assert_eq!(
        count(&state, "SELECT last_value FROM preview_attempts").await,
        2
    );
}
#[tokio::test]
async fn failed_preview_insert_rolls_back_expired_preview_collection() {
    let state = fixture().await;
    let Json(old) = preview(&state).await.unwrap();
    sqlx::query("UPDATE authorization_group_previews SET expires_at=now()-interval '1 hour'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_preview_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected failure'; END $$; CREATE TRIGGER reject_preview_insert AFTER INSERT ON authorization_group_previews FOR EACH ROW EXECUTE FUNCTION reject_preview_insert();").execute(&state.pool).await.unwrap();
    assert!(preview(&state).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_group_previews").await,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT token FROM authorization_group_previews")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        old["preview_token"].as_str().unwrap()
    );
}
#[tokio::test]
async fn preview_collection_is_bounded_and_skips_claimed_rows() {
    let state = fixture().await;
    let Json(receipt) = preview(&state).await.unwrap();
    let token = receipt["preview_token"].as_str().unwrap();
    sqlx::query("INSERT INTO authorization_group_previews(token,action,request_json,guard_json,result_json,created_at,expires_at) SELECT 'expired-'||i,action,request_json,guard_json,result_json,created_at,now()-interval '1 hour' FROM authorization_group_previews CROSS JOIN generate_series(1,300) i WHERE token=$1").bind(token).execute(&state.pool).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query(
        "SELECT token FROM authorization_group_previews WHERE token='expired-1' FOR UPDATE",
    )
    .fetch_one(&mut *held)
    .await
    .unwrap();
    let _ = preview(&state).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE expires_at<now()"
        )
        .await,
        44
    );
    held.rollback().await.unwrap();
    let _ = preview(&state).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE expires_at<now()"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn database_expiry_is_shared_by_preparation_and_commit_validation() {
    let state = fixture().await;
    let Json(receipt) = preview(&state).await.unwrap();
    let token = receipt["preview_token"].as_str().unwrap();
    assert!(
        crate::credentials::authorization::prepare_preview(&state.pool, &state.secrets, token)
            .await
            .is_ok()
    );
    assert!(sqlx::query_scalar::<_,bool>("SELECT expires_at-created_at=interval '10 minutes' FROM authorization_group_previews WHERE token=$1").bind(token).fetch_one(&state.pool).await.unwrap());
    sqlx::query("UPDATE authorization_group_previews SET expires_at=clock_timestamp()-interval '1 second' WHERE token=$1").bind(token).execute(&state.pool).await.unwrap();
    assert!(
        crate::credentials::authorization::prepare_preview(&state.pool, &state.secrets, token)
            .await
            .is_err()
    );
    let mut tx = state.pool.begin().await.unwrap();
    assert_eq!(
        load_preview(&mut tx, token).await.unwrap_err().status,
        StatusCode::CONFLICT
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE consumed_at IS NOT NULL"
        )
        .await,
        0
    );
}

async fn grant_input(state: &AppState) -> CreateGroupRequest {
    let Json(receipt) = preview(state).await.unwrap();
    CreateGroupRequest {
        name: "Group".into(),
        user_ids: vec!["user".into()],
        node_ids: vec!["node".into()],
        preview_token: receipt["preview_token"].as_str().unwrap().into(),
    }
}
async fn clean_fixture() -> AppState {
    let state = fixture().await;
    sqlx::query("DELETE FROM node_assignments")
        .execute(&state.pool)
        .await
        .unwrap();
    state
}
async fn grant(state: &AppState, input: CreateGroupRequest) -> Result<Value, ApiError> {
    create(State(state.clone()), Json(input))
        .await
        .map(|(_, Json(value))| value)
}
#[tokio::test]
async fn failed_grant_audit_preserves_preview_user_and_assignment_state() {
    let state = clean_fixture().await;
    let input = grant_input(&state).await;
    let user: Value = sqlx::query_scalar("SELECT to_jsonb(u) FROM users u WHERE id='user'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_grant_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='authorization_group.created' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_grant_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_grant_audit();").execute(&state.pool).await.unwrap();
    assert!(grant(&state, input).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_groups").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE consumed_at IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(u) FROM users u WHERE id='user'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        user
    );
}
#[tokio::test]
async fn retried_grant_returns_the_final_assignment_secret_and_one_audit() {
    let state = clean_fixture().await;
    let input = grant_input(&state).await;
    sqlx::raw_sql("CREATE SEQUENCE grant_attempts; CREATE FUNCTION retry_grant_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='authorization_group.created' AND nextval('grant_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_grant_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_grant_audit();").execute(&state.pool).await.unwrap();
    let legacy = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    let receipt = super::super::AUDIT_ACTOR_ID
        .scope("group-operator".into(), grant(&state, input))
        .await
        .unwrap();
    legacy.rollback().await.unwrap();
    let secret = receipt["created_credentials"][0]["hy2_credential"]
        .as_str()
        .unwrap();
    let (cipher,hash):(String,String)=sqlx::query_as("SELECT credential_enc,credential_hash FROM node_assignments WHERE user_id='user' AND node_id='node'").fetch_one(&state.pool).await.unwrap();
    assert_eq!(state.secrets.decrypt(&cipher).unwrap(), secret);
    assert_eq!(hash, token_digest(secret));
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_groups").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE consumed_at IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM audit_records WHERE action='authorization_group.created' AND actor='group-operator'").await,1);
    assert_eq!(
        count(&state, "SELECT last_value FROM grant_attempts").await,
        2
    );
}
#[tokio::test]
async fn concurrent_reuse_of_one_preview_commits_one_grant() {
    let state = clean_fixture().await;
    let input = grant_input(&state).await;
    let (a, b) = tokio::join!(grant(&state, input.clone()), grant(&state, input));
    assert_eq!([&a, &b].iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        [&a, &b]
            .iter()
            .filter(|r| r
                .as_ref()
                .err()
                .is_some_and(|e| e.status == StatusCode::CONFLICT))
            .count(),
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_groups").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        1
    );
}
async fn change_preview(state: &AppState, id: &str, action: &str) -> Value {
    let request = if action == "delete" {
        json!({"action":action,"expected_revision":1})
    } else {
        json!({"action":action,"expected_revision":1,"name":"Renamed","user_ids":["user"],"node_ids":["node"]})
    };
    let Json(receipt) = preview_group(
        State(state.clone()),
        Path(id.into()),
        Json(serde_json::from_value(request).unwrap()),
    )
    .await
    .unwrap();
    receipt
}
#[tokio::test]
async fn edit_and_delete_audit_failures_preserve_group_and_preview() {
    let state = clean_fixture().await;
    let input = grant_input(&state).await;
    let created = grant(&state, input).await.unwrap();
    let id = created["group"]["id"].as_str().unwrap();
    let edit = change_preview(&state, id, "update").await;
    let removal = change_preview(&state, id, "delete").await;
    sqlx::raw_sql("CREATE FUNCTION reject_group_change_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action IN ('authorization_group.updated','authorization_group.deleted') THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_group_change_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_group_change_audit();").execute(&state.pool).await.unwrap();
    assert!(
        update(
            State(state.clone()),
            Path(id.into()),
            Json(UpdateGroupRequest {
                expected_revision: 1,
                name: "Renamed".into(),
                user_ids: vec!["user".into()],
                node_ids: vec!["node".into()],
                preview_token: edit["preview_token"].as_str().unwrap().into()
            })
        )
        .await
        .is_err()
    );
    assert!(
        delete(
            State(state.clone()),
            Path(id.into()),
            Json(DeleteGroupRequest {
                expected_revision: 1,
                preview_token: removal["preview_token"].as_str().unwrap().into()
            })
        )
        .await
        .is_err()
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_groups WHERE name='Group' AND revision=1"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE consumed_at IS NULL"
        )
        .await,
        2
    );
}
async fn membership_input(state: &AppState, groups: Vec<String>) -> MembershipUpdateRequest {
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM users WHERE id='user'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let Json(receipt) = preview_memberships(
        State(state.clone()),
        Path("user".into()),
        Json(MembershipPreviewRequest {
            expected_revision: revision,
            group_ids: groups.clone(),
        }),
    )
    .await
    .unwrap();
    MembershipUpdateRequest {
        expected_revision: revision,
        group_ids: groups,
        preview_token: receipt["preview_token"].as_str().unwrap().into(),
    }
}
#[tokio::test]
async fn membership_removal_retries_one_revocation_and_consumption() {
    let state = clean_fixture().await;
    let input = grant_input(&state).await;
    let _ = grant(&state, input).await.unwrap();
    let input = membership_input(&state, vec![]).await;
    sqlx::raw_sql("CREATE SEQUENCE membership_attempts; CREATE FUNCTION retry_membership_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='authorization_group.user_membership_updated' AND nextval('membership_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_membership_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_membership_audit();").execute(&state.pool).await.unwrap();
    let Json(receipt) = update_memberships(State(state.clone()), Path("user".into()), Json(input))
        .await
        .unwrap();
    assert_eq!(receipt["revocation_job_ids"].as_array().unwrap().len(), 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE consumed_at IS NULL"
        )
        .await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM audit_records WHERE action='authorization_group.user_membership_updated'").await,1);
    assert_eq!(
        count(&state, "SELECT last_value FROM membership_attempts").await,
        2
    );
}

#[tokio::test]
async fn failed_membership_audit_preserves_assignments_and_does_not_enqueue_revocation() {
    let state = clean_fixture().await;
    let input = grant_input(&state).await;
    let _ = grant(&state, input).await.unwrap();
    let input = membership_input(&state, vec![]).await;
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(u) FROM users u WHERE id='user'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_membership_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='authorization_group.user_membership_updated' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_membership_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_membership_audit();").execute(&state.pool).await.unwrap();
    assert!(
        update_memberships(State(state.clone()), Path("user".into()), Json(input))
            .await
            .is_err()
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE consumed_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(u) FROM users u WHERE id='user'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        before
    );
}
#[tokio::test]
async fn grant_waiting_for_a_user_edit_revalidates_the_preview_snapshot() {
    let state = clean_fixture().await;
    let input = grant_input(&state).await;
    let mut editing = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE users SET revision=revision+1,name='Edited' WHERE id='user'")
        .execute(&mut *editing)
        .await
        .unwrap();
    let app = state.clone();
    let request = tokio::spawn(async move { grant(&app, input).await });
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'SELECT id FROM users WHERE id=ANY($1) ORDER BY id FOR NO KEY UPDATE%' AND wait_event_type='Lock')").fetch_one(&state.pool).await.unwrap() {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    editing.commit().await.unwrap();
    assert_eq!(
        request.await.unwrap().unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_groups").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE consumed_at IS NULL"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn preview_expiring_while_waiting_for_its_lock_is_rejected() {
    let state = fixture().await;
    let Json(receipt) = preview(&state).await.unwrap();
    let token = receipt["preview_token"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE authorization_group_previews SET expires_at=clock_timestamp()+interval '1 second' WHERE token=$1").bind(&token).execute(&state.pool).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT token FROM authorization_group_previews WHERE token=$1 FOR UPDATE")
        .bind(&token)
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let app = state.clone();
    let request = tokio::spawn(async move {
        let mut tx = app.pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let result = load_preview(&mut tx, &token).await;
        tx.rollback().await.unwrap();
        result
    });
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'WITH held AS MATERIALIZED%' AND wait_event_type='Lock')").fetch_one(&state.pool).await.unwrap() {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    held.rollback().await.unwrap();
    assert_eq!(
        request.await.unwrap().unwrap_err().status,
        StatusCode::CONFLICT
    );
}
