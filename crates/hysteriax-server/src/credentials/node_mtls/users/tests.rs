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
    let (job, leases) = crate::jobs::claim_for_test(&state.pool, &state.instance_id)
        .await
        .unwrap();
    Fixture { state, job, leases }
}
async fn source(f: &Fixture) -> Source {
    let mut s = execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        capture(&f.state.pool, &f.job),
    )
    .await
    .unwrap()
    .unwrap();
    // Membership arrays are only test observations. Production source capture
    // carries a constant-size version fence and each signing page is bounded.
    s.users = sqlx::query_scalar(
        "SELECT user_id FROM node_assignments WHERE node_id=$1 ORDER BY user_id LIMIT 100",
    )
    .bind(&f.job.node_id)
    .fetch_all(&f.state.pool)
    .await
    .unwrap();
    s
}

async fn save_candidates(f: &Fixture, s: &Source, candidates: Vec<PreparedIdentity>) -> Result<()> {
    execution::supervise(f.state.pool.clone(), f.leases.clone(), async {
        execution::with_identity_leases(
            &s.identity_ids,
            commit(&f.state.pool, &f.state.secrets, s, candidates),
        )
        .await?
        .context("identity page busy")
    })
    .await
    .context("job scope lost")?
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
async fn preparation_and_final_source_check_preserve_sources() {
    let f = fixture().await;
    let before = snapshot(&f).await;
    let s = source(&f).await;
    save(&f, &s).await.unwrap();
    assert_eq!(snapshot(&f).await, before);
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
    let s = execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        pending_page(&f.state.pool, &s, ca, 1),
    )
    .await
    .unwrap()
    .unwrap();
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
    assert!(save_candidates(&f, &s, candidates.clone()).await.is_err());
    assert_eq!(snapshot(&f).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_deployment_identity ON credentials; ALTER SEQUENCE deployment_identity_attempt RESTART WITH 1; CREATE FUNCTION retry_deployment_identity() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.owner_user_id IS NOT NULL AND nextval('deployment_identity_attempt')=2 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_deployment_identity BEFORE INSERT ON credentials FOR EACH ROW EXECUTE FUNCTION retry_deployment_identity()")
        .execute(&f.state.pool).await.unwrap();
    save_candidates(&f, &s, candidates).await.unwrap();
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

async fn generated_ca(f: &Fixture) -> String {
    use axum::{Json, extract::State};
    let (_, Json(result)) = crate::api::managed_mtls::create(
        State(f.state.clone()),
        fixture_api::actor(),
        Json(crate::api::managed_mtls::CreateAuthority {
            name: "paged deployment CA".into(),
            source: crate::api::managed_mtls::Source::Generated,
            certificate: None,
            private_key: None,
            policy: crate::credentials::ca::Policy::default(),
            idempotency_key: "paged-ca".into(),
        }),
    )
    .await
    .unwrap();
    result["id"].as_str().unwrap().to_owned()
}
async fn assign_many(f: &Fixture, count: i64) {
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) SELECT 'user-'||lpad(i::text,4,'0'),'Owner '||i,true,clock_timestamp(),clock_timestamp() FROM generate_series(1,$1::bigint) i")
        .bind(count).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) SELECT id,$1,id,$2,clock_timestamp() FROM users")
        .bind(&f.job.node_id).bind(f.state.secrets.encrypt("fixture").unwrap()).execute(&f.state.pool).await.unwrap();
}
async fn run_prepare(f: &Fixture, ca: &str) -> Result<()> {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        prepare(&f.state.pool, &f.state.secrets, &f.job, ca, 1),
    )
    .await
    .context("job scope lost")?
}
async fn leaves(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM mtls_user_versions")
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
}
async fn first_page_ciphertexts(f: &Fixture) -> Value {
    sqlx::query_scalar("SELECT jsonb_agg(v.payload_enc ORDER BY i.user_id,v.version) FROM mtls_user_identities i JOIN credential_versions v ON v.credential_id=i.credential_id WHERE i.user_id<='user-0100'")
        .fetch_one(&f.state.pool).await.unwrap()
}

#[tokio::test]
async fn deployment_preparation_commits_hundred_owner_pages_revisits_newly_enabled_earlier_owners_and_rollback_only_reads()
 {
    let f = fixture().await;
    let ca = generated_ca(&f).await;
    assign_many(&f, 201).await;
    sqlx::query("UPDATE users SET enabled=false WHERE id='user-0001'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TABLE observed_identity_pages(transaction_id bigint PRIMARY KEY,items bigint CHECK(items<=100)); CREATE FUNCTION observe_identity_page() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO observed_identity_pages VALUES(txid_current(),1) ON CONFLICT(transaction_id) DO UPDATE SET items=observed_identity_pages.items+1; IF (SELECT user_id FROM mtls_user_identities WHERE id=NEW.identity_id)='user-0101' THEN UPDATE users SET enabled=true WHERE id='user-0001'; END IF; RETURN NEW; END $$; CREATE TRIGGER observe_identity_page AFTER INSERT ON mtls_user_versions FOR EACH ROW EXECUTE FUNCTION observe_identity_page();")
        .execute(&f.state.pool).await.unwrap();
    run_prepare(&f, &ca).await.unwrap();
    assert_eq!(leaves(&f).await, 201);
    let pages: Vec<i64> =
        sqlx::query_scalar("SELECT items FROM observed_identity_pages ORDER BY transaction_id")
            .fetch_all(&f.state.pool)
            .await
            .unwrap();
    assert_eq!(pages, vec![99, 100, 2]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users WHERE revision<>1")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM resource_leases WHERE resource_key LIKE 'identity:%' AND owner IS NOT NULL").fetch_one(&f.state.pool).await.unwrap(),0);
    let before = snapshot(&f).await;
    run_prepare(&f, &ca).await.unwrap();
    assert_eq!(
        snapshot(&f).await,
        before,
        "healthy shared material is durable progress, not another signing request"
    );
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        validate_rollback(&f.state.pool, &f.job, &ca, 1, &[1]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        snapshot(&f).await,
        before,
        "rollback cannot issue replacement material"
    );
    sqlx::query("UPDATE mtls_user_versions SET discarded=true WHERE identity_id=(SELECT id FROM mtls_user_identities WHERE user_id='user-0201')")
        .execute(&f.state.pool).await.unwrap();
    let before = snapshot(&f).await;
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            f.leases.clone(),
            validate_rollback(&f.state.pool, &f.job, &ca, 1, &[1])
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(snapshot(&f).await, before);
}

#[tokio::test]
async fn later_page_failure_and_new_job_incarnation_resume_committed_certificates_without_reissuing_them()
 {
    let mut f = fixture().await;
    let ca = generated_ca(&f).await;
    assign_many(&f, 201).await;
    sqlx::raw_sql("CREATE FUNCTION reject_later_identity_page() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF (SELECT user_id FROM mtls_user_identities WHERE id=NEW.identity_id)='user-0101' THEN RAISE EXCEPTION 'reject later page'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_later_identity_page BEFORE INSERT ON mtls_user_versions FOR EACH ROW EXECUTE FUNCTION reject_later_identity_page();")
        .execute(&f.state.pool).await.unwrap();
    assert!(run_prepare(&f, &ca).await.is_err());
    assert_eq!(leaves(&f).await, 100);
    let original = first_page_ciphertexts(&f).await;
    sqlx::query("DROP TRIGGER reject_later_identity_page ON mtls_user_versions")
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1 AND owner IS NOT NULL")
        .bind(&f.job.id).execute(&f.state.pool).await.unwrap();
    let mut tx = f.state.pool.begin().await.unwrap();
    let mut next = vec![];
    for old in &f.leases {
        next.push(
            execution::acquire(&mut tx, &old.resource, "next-incarnation", &f.job.id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    sqlx::query("UPDATE jobs SET attempts=attempts+1 WHERE id=$1")
        .bind(&f.job.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    f.leases = next;
    f.job.attempts += 1;
    run_prepare(&f, &ca).await.unwrap();
    assert_eq!(leaves(&f).await, 201);
    assert_eq!(first_page_ciphertexts(&f).await, original);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM mtls_user_identities WHERE credential_id IS NOT NULL"
        )
        .fetch_one(&f.state.pool)
        .await
        .unwrap(),
        201
    );
}

#[tokio::test]
async fn a_busy_shared_identity_rolls_back_partial_claims_and_never_polls_the_signer() {
    use crate::background::maintenance;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = fixture().await;
    maintenance::ensure(
        &f.state.pool,
        "competing-renewal",
        "test-identity-renewal",
        json!({"lease_resources":["identity:z"]}),
    )
    .await
    .unwrap();
    let work = maintenance::claim(&f.state.pool, "test-identity-renewal", "regular-renewal")
        .await
        .unwrap()
        .unwrap();
    let polls = AtomicUsize::new(0);
    let result = execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        execution::with_identity_leases(&["a".into(), "z".into()], async {
            polls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.is_none());
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM resource_leases WHERE resource_key='identity:a')"
        )
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT owner FROM resource_leases WHERE resource_key='identity:z'"
        )
        .fetch_one(&f.state.pool)
        .await
        .unwrap(),
        "regular-renewal"
    );
    execution::supervise(
        f.state.pool.clone(),
        work.leases.clone(),
        db::write::<_, anyhow::Error, _>(&f.state.pool, move |tx| {
            let work = work.clone();
            Box::pin(async move { work.finish_on(tx, false, 0, None).await })
        }),
    )
    .await
    .unwrap()
    .unwrap();
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        execution::with_identity_leases(&["a".into(), "z".into()], async { Ok(()) }),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn identity_claim_checks_lexically_earlier_identity_before_waiting_on_the_inherited_job_row()
{
    let f = fixture().await;
    sqlx::query("INSERT INTO resource_leases(resource_key) VALUES('identity:busy')")
        .execute(&f.state.pool)
        .await
        .unwrap();
    execution::supervise(f.state.pool.clone(),f.leases.clone(),async {
        let mut held=f.state.pool.begin().await.unwrap();
        sqlx::query("SELECT resource_key FROM resource_leases WHERE resource_key='identity:busy' OR resource_key=$1 ORDER BY resource_key FOR UPDATE")
            .bind(format!("job:{}",f.job.id)).fetch_all(&mut *held).await.unwrap();
        let result=tokio::time::timeout(std::time::Duration::from_secs(3),execution::with_identity_leases(&["busy".into()],async {panic!("busy signer polled");#[allow(unreachable_code)] Ok(())})).await.unwrap();
        held.rollback().await.unwrap();
        assert!(result.unwrap().is_none(),"a parent-first guard would have blocked on the job row before observing the busy identity");
    }).await.unwrap();
}

#[tokio::test]
async fn identity_claim_and_release_retry_native_conflicts_without_polling_the_signer_again() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = fixture().await;
    sqlx::raw_sql("CREATE SEQUENCE identity_claim_attempt; CREATE SEQUENCE identity_release_attempt; CREATE FUNCTION retry_identity_scope() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.resource_key='identity:retry' THEN IF OLD.owner IS NULL AND NEW.owner IS NOT NULL AND nextval('identity_claim_attempt')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry identity claim'; END IF; IF OLD.owner IS NOT NULL AND NEW.owner IS NULL AND nextval('identity_release_attempt')=1 THEN RAISE EXCEPTION USING ERRCODE='40P01',MESSAGE='retry identity release'; END IF; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_identity_scope BEFORE UPDATE ON resource_leases FOR EACH ROW EXECUTE FUNCTION retry_identity_scope();")
        .execute(&f.state.pool).await.unwrap();
    let polls = AtomicUsize::new(0);
    let result = execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        execution::with_identity_leases(&["retry".into()], async {
            polls.fetch_add(1, Ordering::SeqCst);
            Ok(42)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, Some(42));
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    for sequence in ["identity_claim_attempt", "identity_release_attempt"] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                "SELECT last_value FROM {sequence}"
            )))
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
            2
        );
    }
}

#[tokio::test]
async fn replaced_identity_lease_rejects_late_database_results_and_is_not_released_by_the_old_signer()
 {
    let f = fixture().await;
    sqlx::query("CREATE TABLE late_identity_result(value bigint)")
        .execute(&f.state.pool)
        .await
        .unwrap();
    let result=execution::supervise(f.state.pool.clone(),f.leases.clone(),execution::with_identity_leases(&["replace".into()],async {
        sqlx::query("UPDATE resource_leases SET owner='replacement',generation=generation+1,work_id='replacement-work' WHERE resource_key='identity:replace'").execute(&f.state.pool).await?;
        db::write::<_,anyhow::Error,_>(&f.state.pool,|tx|Box::pin(async move {
            sqlx::query("INSERT INTO late_identity_result VALUES(1)").execute(&mut **tx).await?;Ok(())
        })).await
    })).await.unwrap();
    assert!(result.is_err() || result.is_ok_and(|v| v.is_none()));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM late_identity_result")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT owner FROM resource_leases WHERE resource_key='identity:replace'"
        )
        .fetch_one(&f.state.pool)
        .await
        .unwrap(),
        "replacement"
    );
}

#[tokio::test]
async fn two_deployments_share_one_identity_generation_and_all_pages_converge_without_duplicate_issuance()
 {
    let f = fixture().await;
    let ca = generated_ca(&f).await;
    assign_many(&f, 101).await;
    let ssh = fixture_api::create(&f.state, "ssh_password", json!({"secret":"fixture"})).await;
    let second_node = fixture_api::node(&f.state, "second signing node", &ssh, json!({})).await;
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) SELECT id,$1,id,$2,clock_timestamp() FROM users")
        .bind(&second_node).bind(f.state.secrets.encrypt("fixture").unwrap()).execute(&f.state.pool).await.unwrap();
    let mut tx = f.state.pool.begin().await.unwrap();
    crate::api::enqueue_job_in_tx(&mut tx, "sync", Some(&second_node), Some(1))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (job, leases) = crate::jobs::claim_for_test(&f.state.pool, "second-signing-instance")
        .await
        .unwrap();
    assert_eq!(job.node_id.as_deref(), Some(second_node.as_str()));
    let second = Fixture {
        state: f.state.clone(),
        job,
        leases,
    };
    let (first_result, second_result) =
        tokio::join!(run_prepare(&f, &ca), run_prepare(&second, &ca));
    first_result.unwrap();
    second_result.unwrap();
    assert_eq!(leaves(&f).await, 101);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mtls_user_identities")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        101
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM credentials WHERE owner_user_id IS NOT NULL AND latest_version<>1"
        )
        .fetch_one(&f.state.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM resource_leases WHERE resource_key LIKE 'identity:%' AND owner IS NOT NULL").fetch_one(&f.state.pool).await.unwrap(),0);
}

#[tokio::test]
async fn candidate_from_another_owner_or_without_the_shared_identity_scope_cannot_publish() {
    let f = fixture().await;
    let ca = generated_ca(&f).await;
    assign(&f, "alice", f.job.node_id.as_deref().unwrap()).await;
    sqlx::query(
        "INSERT INTO users(id,name,created_at,updated_at) VALUES('other','Other',now(),now())",
    )
    .execute(&f.state.pool)
    .await
    .unwrap();
    let original = source(&f).await;
    let page = execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        pending_page(&f.state.pool, &original, &ca, 1),
    )
    .await
    .unwrap()
    .unwrap();
    let wrong = identities::prepare_user(&f.state.pool, &f.state.secrets, "other", &ca, 1, false)
        .await
        .unwrap();
    let before = snapshot(&f).await;
    assert!(save_candidates(&f, &page, vec![wrong]).await.is_err());
    assert_eq!(snapshot(&f).await, before);
    let right = identities::prepare_user(&f.state.pool, &f.state.secrets, "alice", &ca, 1, false)
        .await
        .unwrap();
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            f.leases.clone(),
            commit(&f.state.pool, &f.state.secrets, &page, vec![right])
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(snapshot(&f).await, before);
    assert_eq!(leaves(&f).await, 0);
}
