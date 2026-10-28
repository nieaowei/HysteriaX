use super::*;
use crate::execution::{self, Execution};

async fn fixture() -> (PgPool, SecretBox, Vec<Execution>) {
    let pool = db::test_pool_with_max_connections(8).await;
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT id,id,'127.0.0.1',22,'root','password','unused','example.test',443,':443',id,'unused','unused','{}',clock_timestamp(),clock_timestamp() FROM unnest(ARRAY['a','b'])id").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,attempts,available_at,created_at,updated_at) VALUES('one','deploy','a',1,'running','starting',1,clock_timestamp(),clock_timestamp(),clock_timestamp()),('two','deploy','b',1,'running','starting',1,clock_timestamp(),clock_timestamp(),clock_timestamp())").execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let mut leases = vec![];
    for key in ["job:one", "node:a"] {
        leases.push(
            execution::acquire(&mut tx, key, "owner", "one")
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    (
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
        leases,
    )
}
fn candidate(secrets: &SecretBox) -> Prepared {
    prepare(
        secrets,
        Begin {
            node: "a",
            job: "one",
            revision: 1,
            digest: "target",
            snapshot: &json!({"server_config":{}}),
            trust: &[],
            probe: None,
        },
    )
    .unwrap()
}
async fn start(pool: &PgPool, secrets: &SecretBox, leases: &[Execution]) -> Record {
    execution::supervise(
        pool.clone(),
        leases.to_vec(),
        begin_prepared(pool, candidate(secrets)),
    )
    .await
    .unwrap()
    .unwrap()
}
async fn snapshot(pool: &PgPool) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'activations',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM mtls_activations a),'trust',(SELECT jsonb_agg(to_jsonb(t) ORDER BY activation_id,ca_id,ca_version) FROM mtls_activation_trust t))").fetch_one(pool).await.unwrap()
}
async fn count(pool: &PgPool, q: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(q))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn native_begin_retry_reuses_the_exact_prepared_id_cipher_and_single_database_generation() {
    let (pool, secrets, leases) = fixture().await;
    let prepared = candidate(&secrets);
    let expected = prepared.clone();
    sqlx::raw_sql("CREATE SEQUENCE activation_attempts; CREATE FUNCTION retry_activation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('activation_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry activation'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_activation BEFORE INSERT ON mtls_activations FOR EACH ROW EXECUTE FUNCTION retry_activation();").execute(&pool).await.unwrap();
    let result = execution::supervise(pool.clone(), leases, begin_prepared(&pool, prepared))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.id, expected.id);
    assert_eq!(result.generation, 1);
    assert!(result.fresh);
    let row = sqlx::query(
        "SELECT snapshot_enc,created_at=updated_at AS same_time FROM mtls_activations WHERE id=$1",
    )
    .bind(expected.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("snapshot_enc"), expected.cipher);
    assert!(row.get::<bool, _>("same_time"));
    assert_eq!(
        count(&pool, "SELECT last_value FROM activation_attempts").await,
        2
    );
    assert_eq!(
        count(&pool, "SELECT mtls_generation FROM nodes WHERE id='a'").await,
        1
    );
}

#[tokio::test]
async fn begin_and_transition_do_not_wait_for_unrelated_nodes() {
    let (pool, secrets, leases) = fixture().await;
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='b' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        start(&pool, &secrets, &leases),
    )
    .await
    .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        execution::supervise(pool.clone(), leases, verified(&pool, "one")),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM mtls_activations WHERE phase='verified'"
        )
        .await,
        1
    );
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn pointer_failure_rolls_back_activation_material_and_node_generation() {
    let (pool, secrets, leases) = fixture().await;
    let before = snapshot(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION reject_activation_pointer() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.mtls_generation<>OLD.mtls_generation THEN RAISE EXCEPTION 'reject pointer'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_activation_pointer BEFORE UPDATE ON nodes FOR EACH ROW EXECUTE FUNCTION reject_activation_pointer();").execute(&pool).await.unwrap();
    assert!(
        execution::supervise(
            pool.clone(),
            leases,
            begin_prepared(&pool, candidate(&secrets))
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
}

#[tokio::test]
async fn resume_preserves_original_ciphertext_and_rejects_changed_target_probe_or_generation() {
    let (pool, secrets, leases) = fixture().await;
    let first = start(&pool, &secrets, &leases).await;
    let before = snapshot(&pool).await;
    let resumed = start(&pool, &secrets, &leases).await;
    assert_eq!(first.id, resumed.id);
    assert!(!resumed.fresh);
    assert_eq!(before, snapshot(&pool).await);
    for mode in 0..3 {
        let mut input = candidate(&secrets);
        match mode {
            0 => input.digest = "different".into(),
            1 => input.revision = 2,
            _ => input.probe = Some(("different".into(), 1)),
        }
        assert!(
            execution::supervise(pool.clone(), leases.clone(), begin_prepared(&pool, input))
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(before, snapshot(&pool).await);
    }
    sqlx::query("UPDATE nodes SET mtls_generation=2 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    let before = snapshot(&pool).await;
    assert!(
        execution::supervise(
            pool.clone(),
            leases,
            begin_prepared(&pool, candidate(&secrets))
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
}

#[tokio::test]
async fn uncertainty_acknowledgement_rolls_back_both_states_and_native_retry_commits_together() {
    let (pool, secrets, leases) = fixture().await;
    start(&pool, &secrets, &leases).await;
    let before = snapshot(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION reject_uncertain_node() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.mtls_state='recovery_required' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_uncertain_node BEFORE UPDATE ON nodes FOR EACH ROW EXECUTE FUNCTION reject_uncertain_node();").execute(&pool).await.unwrap();
    assert!(
        execution::supervise(pool.clone(), leases.clone(), uncertain(&pool, "one"))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
    sqlx::raw_sql("DROP TRIGGER reject_uncertain_node ON nodes; CREATE SEQUENCE uncertain_attempts; CREATE FUNCTION retry_uncertain_node() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.mtls_state='recovery_required' AND nextval('uncertain_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40P01',MESSAGE='retry'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_uncertain_node BEFORE UPDATE ON nodes FOR EACH ROW EXECUTE FUNCTION retry_uncertain_node();").execute(&pool).await.unwrap();
    execution::supervise(pool.clone(), leases, uncertain(&pool, "one"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(count(&pool,"SELECT count(*) FROM mtls_activations a JOIN nodes n ON n.mtls_activation_id=a.id WHERE a.phase='recovery_required' AND n.mtls_state='recovery_required'").await,1);
    assert_eq!(
        count(&pool, "SELECT last_value FROM uncertain_attempts").await,
        2
    );
}

#[tokio::test]
async fn stale_generation_or_pointer_cannot_verify_mark_uncertain_or_rollback() {
    let (pool, secrets, leases) = fixture().await;
    start(&pool, &secrets, &leases).await;
    for mode in 0..2 {
        sqlx::query("UPDATE nodes SET mtls_generation=$1,mtls_activation_id=CASE WHEN $1=1 THEN NULL ELSE (SELECT id FROM mtls_activations WHERE job_id='one') END WHERE id='a'").bind(if mode==0 {2_i64}else{1_i64}).execute(&pool).await.unwrap();
        let before = snapshot(&pool).await;
        for change in [
            Transition::Verified,
            Transition::Uncertain,
            Transition::RolledBack(None),
        ] {
            assert!(
                execution::supervise(
                    pool.clone(),
                    leases.clone(),
                    transition(&pool, "one", change)
                )
                .await
                .unwrap()
                .is_err()
            );
            assert_eq!(before, snapshot(&pool).await);
        }
    }
}

#[tokio::test]
async fn rollback_proof_is_required_and_repeated_ack_preserves_monotone_generation_and_terminal_material()
 {
    let (pool, secrets, leases) = fixture().await;
    start(&pool, &secrets, &leases).await;
    let before = snapshot(&pool).await;
    assert!(
        execution::supervise(
            pool.clone(),
            leases.clone(),
            rolled_back(&pool, "one", Some("wrong"))
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
    execution::supervise(
        pool.clone(),
        leases.clone(),
        rolled_back(&pool, "one", None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        count(&pool, "SELECT mtls_generation FROM nodes WHERE id='a'").await,
        1
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT mtls_activation_id IS NULL AND mtls_state='ready' FROM nodes WHERE id='a'"
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    );
    let before = snapshot(&pool).await;
    execution::supervise(
        pool.clone(),
        leases.clone(),
        rolled_back(&pool, "one", None),
    )
    .await
    .unwrap()
    .unwrap();
    execution::supervise(pool.clone(), leases.clone(), uncertain(&pool, "one"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before, snapshot(&pool).await);
    assert!(
        execution::supervise(pool.clone(), leases, verified(&pool, "one"))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
}

#[tokio::test]
async fn wrong_job_missing_node_resource_cancelled_and_replaced_leases_cannot_write_activation_results()
 {
    let (pool, secrets, leases) = fixture().await;
    start(&pool, &secrets, &leases).await;
    let before = snapshot(&pool).await;
    assert!(
        execution::supervise(pool.clone(), leases.clone(), verified(&pool, "two"))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
    assert!(
        execution::supervise(
            pool.clone(),
            vec![leases[0].clone()],
            verified(&pool, "one")
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let mut next = vec![];
    for lease in &leases {
        next.push(
            execution::acquire(&mut tx, &lease.resource, "next", "one")
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    assert!(
        execution::supervise(pool.clone(), leases, verified(&pool, "one"))
            .await
            .is_none()
    );
    assert_eq!(before, snapshot(&pool).await);
    sqlx::query("UPDATE jobs SET status='cancelled' WHERE id='one'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        execution::supervise(pool.clone(), next, uncertain(&pool, "one"))
            .await
            .is_none()
    );
    assert_eq!(before, snapshot(&pool).await);
}

#[tokio::test]
async fn unresolved_remote_steps_prevent_verification_or_rollback_from_reopening_subscriptions() {
    let (pool, secrets, leases) = fixture().await;
    start(&pool, &secrets, &leases).await;
    let mut tx = pool.begin().await.unwrap();
    leases[1]
        .dispatch(&mut tx, "switch", &json!({"request_digest":"fixed"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = snapshot(&pool).await;
    for change in [Transition::Verified, Transition::RolledBack(None)] {
        assert!(
            execution::supervise(
                pool.clone(),
                leases.clone(),
                transition(&pool, "one", change)
            )
            .await
            .unwrap()
            .is_err()
        );
        assert_eq!(before, snapshot(&pool).await);
    }
    execution::supervise(pool.clone(), leases, uncertain(&pool, "one"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM nodes WHERE id='a' AND mtls_state='recovery_required'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM execution_operations WHERE state='dispatching'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn nested_commit_native_conflict_rolls_back_activation_snapshot_and_trust_then_retries() {
    let (pool, secrets, leases) = fixture().await;
    let record = start(&pool, &secrets, &leases).await;
    execution::supervise(pool.clone(), leases.clone(), verified(&pool, "one"))
        .await
        .unwrap()
        .unwrap();
    sqlx::raw_sql("CREATE SEQUENCE snapshot_attempts; CREATE FUNCTION retry_activation_snapshot() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('snapshot_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry snapshot'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_activation_snapshot BEFORE INSERT ON mtls_deployment_snapshots FOR EACH ROW EXECUTE FUNCTION retry_activation_snapshot();").execute(&pool).await.unwrap();
    execution::supervise(
        pool.clone(),
        leases,
        db::serializable(&pool, |tx| {
            Box::pin(async move { commit(tx, "one", "target").await })
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        count(&pool, "SELECT last_value FROM snapshot_attempts").await,
        2
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM mtls_deployment_snapshots").await,
        1
    );
    assert!(sqlx::query_scalar::<_,bool>("SELECT a.phase='committed' AND n.mtls_state='ready' AND a.updated_at=s.created_at FROM mtls_activations a JOIN nodes n ON n.mtls_activation_id=a.id JOIN mtls_deployment_snapshots s ON s.activation_id=a.id WHERE a.id=$1").bind(record.id).fetch_one(&pool).await.unwrap());
}

#[tokio::test]
async fn recovery_baseline_retry_returns_the_committed_record_without_changing_snapshot_or_generation()
 {
    let (pool, secrets, leases) = fixture().await;
    let record = start(&pool, &secrets, &leases).await;
    let cipher: String =
        sqlx::query_scalar("SELECT snapshot_enc FROM mtls_activations WHERE id=$1")
            .bind(&record.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::raw_sql("CREATE SEQUENCE baseline_attempts; CREATE FUNCTION retry_activation_baseline() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.previous_digest IS NOT NULL AND nextval('baseline_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry baseline'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_activation_baseline BEFORE UPDATE ON mtls_activations FOR EACH ROW EXECUTE FUNCTION retry_activation_baseline();").execute(&pool).await.unwrap();
    let updated = execution::supervise(
        pool.clone(),
        leases.clone(),
        record_previous(&pool, "one", &record, Some("observed")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(updated.previous_digest.as_deref(), Some("observed"));
    assert_eq!(updated.id, record.id);
    assert_eq!(updated.generation, record.generation);
    assert!(updated.fresh);
    assert_eq!(
        count(&pool, "SELECT last_value FROM baseline_attempts").await,
        2
    );
    assert_eq!(
        cipher,
        sqlx::query_scalar::<_, String>("SELECT snapshot_enc FROM mtls_activations WHERE id=$1")
            .bind(&record.id)
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    let before = snapshot(&pool).await;
    assert!(
        execution::supervise(
            pool.clone(),
            leases.clone(),
            record_previous(&pool, "one", &updated, Some("different"))
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
    execution::supervise(
        pool.clone(),
        leases.clone(),
        record_previous(&pool, "one", &record, Some("observed")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(before, snapshot(&pool).await);
    assert!(
        execution::supervise(
            pool.clone(),
            leases,
            record_previous(&pool, "one", &record, Some("different"))
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
}

#[tokio::test]
async fn resumed_or_replaced_activation_cannot_rebind_its_recovery_baseline() {
    let (pool, secrets, leases) = fixture().await;
    start(&pool, &secrets, &leases).await;
    let resumed = start(&pool, &secrets, &leases).await;
    assert!(!resumed.fresh);
    let before = snapshot(&pool).await;
    assert!(
        execution::supervise(
            pool.clone(),
            leases.clone(),
            record_previous(&pool, "one", &resumed, Some("different"))
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
    sqlx::query("UPDATE nodes SET mtls_generation=2 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    let before = snapshot(&pool).await;
    assert!(
        execution::supervise(
            pool.clone(),
            leases,
            record_previous(&pool, "one", &resumed, None)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
}
