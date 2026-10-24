use super::*;
use crate::{credentials::tests as fixture_api, state::AppState};
use serde_json::{Value, json};

struct Fixture {
    state: AppState,
    job: JobInput,
    leases: Vec<Execution>,
}
async fn fixture() -> Fixture {
    let state = fixture_api::state().await;
    let ssh = fixture_api::create(&state, "ssh_password", json!({"secret":"fixture"})).await;
    let node = fixture_api::node(&state, "target", &ssh, json!({})).await;
    let mut tx = state.pool.begin().await.unwrap();
    crate::api::enqueue_job_in_tx(&mut tx, "sync", Some(&node), Some(1))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    let (job, leases) = crate::jobs::claim_for_test(&state.pool, &state.instance_id)
        .await
        .unwrap();
    Fixture { state, job, leases }
}
async fn source(f: &Fixture) -> Source {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        capture(&f.state.pool, &f.job),
    )
    .await
    .unwrap()
    .unwrap()
}
async fn save(f: &Fixture, s: &Source) -> Result<()> {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        validate(&f.state.pool, s),
    )
    .await
    .unwrap()
}
async fn snapshot(f: &Fixture) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'jobs',(SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j),'identities',(SELECT jsonb_agg(to_jsonb(i) ORDER BY id) FROM mtls_user_identities i),'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v))").fetch_one(&f.state.pool).await.unwrap()
}

#[tokio::test]
async fn preparation_and_final_source_check_do_not_wait_for_the_old_global_lock() {
    let f = fixture().await;
    let legacy = db::begin_write(&f.state.pool).await.unwrap();
    let before = snapshot(&f).await;
    let s = source(&f).await;
    save(&f, &s).await.unwrap();
    assert_eq!(snapshot(&f).await, before);
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn changed_node_deleted_node_job_payload_or_attempt_rejects_old_preparation() {
    for query in [
        "UPDATE nodes SET desired_revision=desired_revision+1",
        "UPDATE nodes SET state='deleting'",
        "UPDATE jobs SET payload_json='{\"force\":true}' WHERE kind='sync'",
        "UPDATE jobs SET attempts=attempts+1 WHERE kind='sync'",
    ] {
        let f = fixture().await;
        let s = source(&f).await;
        sqlx::query(query).execute(&f.state.pool).await.unwrap();
        let before = snapshot(&f).await;
        let result = execution::supervise(
            f.state.pool.clone(),
            f.leases.clone(),
            validate(&f.state.pool, &s),
        )
        .await;
        assert!(result.is_none_or(|r| r.is_err()), "{query}");
        assert_eq!(snapshot(&f).await, before);
    }
}

#[tokio::test]
async fn takeover_cannot_reuse_old_scope_and_current_owner_can_revalidate() {
    let mut f = fixture().await;
    let old = source(&f).await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    let mut tx = f.state.pool.begin().await.unwrap();
    let mut leases = vec![];
    for l in &f.leases {
        leases.push(
            execution::acquire(&mut tx, &l.resource, "next", &f.job.id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    f.leases = leases;
    let before = snapshot(&f).await;
    assert!(save(&f, &old).await.is_err());
    assert_eq!(snapshot(&f).await, before);
    let current = source(&f).await;
    save(&f, &current).await.unwrap();
}

#[tokio::test]
async fn unknown_remote_steps_and_missing_node_scope_reject_preparation() {
    let f = fixture().await;
    let only_job = f
        .leases
        .iter()
        .find(|l| l.resource.starts_with("job:"))
        .unwrap()
        .clone();
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            vec![only_job],
            capture(&f.state.pool, &f.job)
        )
        .await
        .unwrap()
        .is_err()
    );
    let s = source(&f).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    f.leases[0]
        .dispatch(&mut tx, "unknown", &json!({}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &s).await.is_err());
    assert_eq!(snapshot(&f).await, before);
}

#[tokio::test]
async fn signed_candidates_roll_back_together_and_native_retry_reuses_the_prepared_batch() {
    use axum::{Json, extract::State};
    let f = fixture().await;
    let (_, Json(ca)) = crate::api::managed_mtls::create(
        State(f.state.clone()),
        fixture_api::actor(),
        Json(crate::api::managed_mtls::CreateAuthority {
            name: "deployment CA".into(),
            source: crate::api::managed_mtls::Source::Generated,
            certificate: None,
            private_key: None,
            policy: crate::credentials::ca::Policy::default(),
            idempotency_key: "prepare-users".into(),
        }),
    )
    .await
    .unwrap();
    let ca = ca["id"].as_str().unwrap();
    for user in ["alice", "bob"] {
        sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES($1,$1,true,clock_timestamp(),clock_timestamp())").bind(user).execute(&f.state.pool).await.unwrap();
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES($1,$2,$1,$3,clock_timestamp())")
            .bind(user).bind(&f.job.node_id).bind(f.state.secrets.encrypt("fixture").unwrap()).execute(&f.state.pool).await.unwrap();
    }
    let s = source(&f).await;
    let mut candidates = vec![];
    for user in &s.users {
        candidates.push(
            identities::prepare_user(&f.state.pool, &f.state.secrets, user, ca, 1, false)
                .await
                .unwrap(),
        );
    }
    sqlx::raw_sql("CREATE SEQUENCE deployment_identity_attempt; CREATE FUNCTION reject_deployment_identity() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.owner_user_id IS NOT NULL AND nextval('deployment_identity_attempt')=2 THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_deployment_identity BEFORE INSERT ON credentials FOR EACH ROW EXECUTE FUNCTION reject_deployment_identity()")
        .execute(&f.state.pool).await.unwrap();
    let before = snapshot(&f).await;
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            f.leases.clone(),
            commit(&f.state.pool, &f.state.secrets, &s, candidates.clone())
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(snapshot(&f).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_deployment_identity ON credentials; ALTER SEQUENCE deployment_identity_attempt RESTART WITH 1; CREATE FUNCTION retry_deployment_identity() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.owner_user_id IS NOT NULL AND nextval('deployment_identity_attempt')=2 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_deployment_identity BEFORE INSERT ON credentials FOR EACH ROW EXECUTE FUNCTION retry_deployment_identity()")
        .execute(&f.state.pool).await.unwrap();
    let legacy = db::begin_write(&f.state.pool).await.unwrap();
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        commit(&f.state.pool, &f.state.secrets, &s, candidates),
    )
    .await
    .unwrap()
    .unwrap();
    legacy.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM deployment_identity_attempt")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        4
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mtls_user_versions")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        2
    );
}

async fn assign(f: &Fixture, user: &str, node: &str) {
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES($1,$1,true,clock_timestamp(),clock_timestamp()) ON CONFLICT DO NOTHING")
        .bind(user).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES($1,$2,$1,$3,clock_timestamp())")
        .bind(user).bind(node).bind(f.state.secrets.encrypt("fixture").unwrap()).execute(&f.state.pool).await.unwrap();
}
async fn generation(f: &Fixture, node: &str) -> i64 {
    sqlx::query_scalar("SELECT generation FROM node_authorization_versions WHERE node_id=$1")
        .bind(node)
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn authorization_fence_rejects_remove_and_readd_even_when_the_same_sorted_members_return() {
    let f = fixture().await;
    let node = f.job.node_id.as_deref().unwrap();
    assign(&f, "alice", node).await;
    let old = source(&f).await;
    sqlx::query("DELETE FROM node_assignments WHERE user_id='alice'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    assign(&f, "alice", node).await;
    assert_eq!(source(&f).await.users, old.users);
    assert!(
        save(&f, &old).await.is_err(),
        "an ABA authorization change must invalidate the old signing source"
    );
    save(&f, &source(&f).await).await.unwrap();
}

#[tokio::test]
async fn rolled_back_membership_and_credential_updates_leave_the_membership_fence_unchanged() {
    let f = fixture().await;
    let node = f.job.node_id.as_deref().unwrap();
    assign(&f, "alice", node).await;
    let old = source(&f).await;
    let before = generation(&f, node).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    sqlx::query("DELETE FROM node_assignments WHERE user_id='alice'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    sqlx::query("UPDATE node_assignments SET credential_hash='new-hash' WHERE user_id='alice'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(generation(&f, node).await, before);
    save(&f, &old).await.unwrap();
}

#[tokio::test]
async fn another_nodes_membership_does_not_invalidate_the_source_and_moves_fence_both_nodes() {
    let f = fixture().await;
    let ssh = fixture_api::create(&f.state, "ssh_password", json!({"secret":"fixture"})).await;
    let other = fixture_api::node(&f.state, "other", &ssh, json!({})).await;
    let node = f.job.node_id.as_deref().unwrap();
    let old = source(&f).await;
    let initial = generation(&f, node).await;
    assign(&f, "alice", &other).await;
    save(&f, &old).await.unwrap();
    let other_before = generation(&f, &other).await;
    sqlx::query("UPDATE node_assignments SET node_id=$1 WHERE user_id='alice'")
        .bind(node)
        .execute(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(generation(&f, node).await, initial + 1);
    assert_eq!(generation(&f, &other).await, other_before + 1);
    assert!(save(&f, &old).await.is_err());
}

#[tokio::test]
async fn truncation_fences_an_empty_source_and_node_deletion_cleans_its_metadata() {
    let f = fixture().await;
    let old = source(&f).await;
    sqlx::query("TRUNCATE node_assignments CASCADE")
        .execute(&f.state.pool)
        .await
        .unwrap();
    assert!(save(&f, &old).await.is_err());
    let ssh = fixture_api::create(&f.state, "ssh_password", json!({"secret":"fixture"})).await;
    let other = fixture_api::node(&f.state, "remove", &ssh, json!({})).await;
    assert_eq!(generation(&f, &other).await, 0);
    assign(&f, "alice", &other).await;
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(&other)
        .execute(&f.state.pool)
        .await
        .unwrap();
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM node_authorization_versions WHERE node_id=$1)"
        )
        .bind(other)
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn authorization_version_migration_backfills_without_rewriting_business_or_encrypted_history()
{
    let pool = db::test_pool_through_version(3, 66).await;
    let state = AppState::new(
        pool,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    crate::credentials::migration::migrate(&state.pool, &state.secrets)
        .await
        .unwrap();
    let ssh = fixture_api::create(
        &state,
        "ssh_password",
        json!({"secret":"unchanged-encrypted-material"}),
    )
    .await;
    let node = fixture_api::node(&state, "retained", &ssh, json!({})).await;
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('retained-user','Retained',now(),now())").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('retained-user',$1,'hash',$2,now())")
        .bind(&node).bind(state.secrets.encrypt("retained-password").unwrap()).execute(&state.pool).await.unwrap();
    let query = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'users',(SELECT jsonb_agg(to_jsonb(u) ORDER BY id) FROM users u),'assignments',(SELECT jsonb_agg(to_jsonb(a) ORDER BY user_id,node_id) FROM node_assignments a),'credentials',(SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM credentials c),'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v),'history',(SELECT jsonb_agg(to_jsonb(h) ORDER BY id) FROM config_versions h),'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM job_events e),'audit',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM audit_records a))";
    let before: Value = sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    db::run_migrations(&state.pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Value>(query)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT generation FROM node_authorization_versions WHERE node_id=$1"
        )
        .bind(node)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
}
