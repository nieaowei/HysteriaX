use super::*;
use crate::{
    api::{AdminActor, managed_mtls},
    security::SecretBox,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
};

pub(in crate::credentials::rotations) async fn fixture(n: i32) -> (AppState, String, String) {
    fixture_with_pool(db::test_pool_with_max_connections(8).await, n).await
}

pub(in crate::credentials::rotations) async fn fixture_with_pool(
    pool: PgPool,
    n: i32,
) -> (AppState, String, String) {
    let state = AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    crate::credentials::migration::migrate(&state.pool, &state.secrets)
        .await
        .unwrap();
    let (_, Json(ca)) = managed_mtls::create(
        State(state.clone()),
        Extension(AdminActor { id: "test".into() }),
        Json(
            serde_json::from_value::<managed_mtls::CreateAuthority>(
                json!({"name":"CA","source":"generated","idempotency_key":"trust-ca-fixture"}),
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    let ca = ca["id"].as_str().unwrap().to_owned();
    sqlx::query("INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('fixture-ssh','SSH','ssh_password',now(),now())").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES('fixture-ssh',1,$1,'{}',now())").bind(state.secrets.encrypt("{\"secret\":\"fixture-only\"}").unwrap()).execute(&state.pool).await.unwrap();
    nodes(&state, &ca, n).await;
    // An identity ensures that even a zero-node fixture has a coordinated
    // generation rather than the unused-CA immediate publication path.
    sqlx::query("INSERT INTO mtls_user_identities(id,user_id,ca_id,created_at,updated_at) VALUES('history','deleted-owner',$1,now(),now())").bind(&ca).execute(&state.pool).await.unwrap();
    let (_, Json(receipt)) = managed_mtls::publish(
        State(state.clone()),
        Extension(AdminActor { id: "test".into() }),
        Path(ca.clone()),
        Json(managed_mtls::PublishAuthority {
            expected_revision: 1,
            certificate: None,
            private_key: None,
            replace_rotation_id: None,
            idempotency_key: "trust-target".into(),
        }),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap().to_owned();
    (state, ca, rotation)
}
pub(in crate::credentials::rotations) async fn nodes(state: &AppState, ca: &str, n: i32) {
    let cipher = state.secrets.encrypt("{}").unwrap();
    sqlx::query("INSERT INTO nodes(id,name,state,ssh_host,ssh_port,ssh_username,ssh_credential_id,ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,mtls_state,deployed_revision,deployed_content_sha256,created_at,updated_at) SELECT 'member-'||lpad(i::text,4,'0'),'Member','deployed','127.0.0.1',9,'root','fixture-ssh',1,'example.test',443,':443','hash-'||i,$2,$2,$2,'ready',1,'fixture-digest',now(),now() FROM generate_series(1,$1)i ON CONFLICT(id) DO NOTHING").bind(n).bind(cipher).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,desired,deployed) SELECT id,$1,1,true,true FROM nodes ON CONFLICT DO NOTHING").bind(ca).execute(&state.pool).await.unwrap();
}
async fn work(state: &AppState, ca: &str, rotation: &str, owner: &str) -> Work {
    let key = format!("rotation-trust:{rotation}");
    maintenance::ensure(&state.pool,&key,KIND,json!({"rotation_id":rotation,"ca_id":ca,"ca_version":2,"cursor":"","lease_resources":[format!("ca:{ca}")]})).await.unwrap();
    maintenance::claim_id(&state.pool, KIND, owner, &format!("maintenance:{key}"))
        .await
        .unwrap()
        .unwrap()
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn three_hundred_one_members_produce_at_most_one_hundred_jobs_per_page_and_keep_the_barrier_closed()
 {
    let (state, ca, rotation) = fixture(301).await;
    for expected in [100, 200, 300, 301] {
        let w = work(&state, &ca, &rotation, "owner").await;
        assert!(!page(&state.pool, &w).await.unwrap());
        assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, expected);
        assert_eq!(
            count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
            expected
        );
        assert_eq!(
            count(&state, "SELECT published_version FROM mtls_authorities").await,
            1
        );
    }
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE payload->>'cursor'='' AND state='complete'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
    // Ordinary revisits preserve healthy pending jobs and do not flood a node.
    let w = work(&state, &ca, &rotation, "next").await;
    assert!(!page(&state.pool, &w).await.unwrap());
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 301);
}

#[tokio::test]
async fn a_busy_early_node_does_not_block_later_members_and_is_revisited_after_wrap() {
    let (state, ca, rotation) = fixture(102).await;
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='member-0001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    for expected in [100, 101] {
        let w = work(&state, &ca, &rotation, "owner").await;
        assert!(
            !tokio::time::timeout(std::time::Duration::from_secs(2), page(&state.pool, &w))
                .await
                .unwrap()
                .unwrap()
        );
        assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, expected);
    }
    held.rollback().await.unwrap();
    let w = work(&state, &ca, &rotation, "revisit").await;
    assert!(!page(&state.pool, &w).await.unwrap());
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 102);
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_rotation_nodes WHERE node_id='member-0001' AND job_id IS NOT NULL").await,1);
}

#[tokio::test]
async fn work_confirmation_failure_rolls_back_jobs_events_members_and_cursor_then_native_retry_commits_once()
 {
    let (state, ca, rotation) = fixture(4).await;
    let w = work(&state, &ca, &rotation, "owner").await;
    let events = count(&state, "SELECT count(*) FROM job_events").await;
    sqlx::raw_sql("CREATE FUNCTION reject_trust_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-trust' AND NEW.state='complete' THEN RAISE EXCEPTION 'confirmation rejected'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_trust_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_trust_ack();").execute(&state.pool).await.unwrap();
    assert!(page(&state.pool, &w).await.is_err());
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_node_operations").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM job_events").await,
        events
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE state='running' AND payload->>'cursor'=''"
        )
        .await,
        1
    );
    sqlx::raw_sql("DROP TRIGGER reject_trust_ack ON background_work; CREATE SEQUENCE trust_attempts; CREATE FUNCTION retry_trust_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-trust' AND NEW.state='complete' AND nextval('trust_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_trust_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_trust_ack();").execute(&state.pool).await.unwrap();
    assert!(!page(&state.pool, &w).await.unwrap());
    assert_eq!(
        count(&state, "SELECT last_value FROM trust_attempts").await,
        2
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 4);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_node_operations").await,
        4
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
        4
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM job_events").await,
        events + 4
    );
}

#[tokio::test]
async fn takeover_preserves_cursor_and_rejects_every_old_owner_write() {
    let (state, ca, rotation) = fixture(101).await;
    let first = work(&state, &ca, &rotation, "first").await;
    page(&state.pool, &first).await.unwrap();
    let old = work(&state, &ca, &rotation, "old").await;
    assert_eq!(old.payload["cursor"], "member-0100");
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = work(&state, &ca, &rotation, "next").await;
    assert_eq!(next.payload, old.payload);
    assert!(page(&state.pool, &old).await.is_err());
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 100);
    assert!(!page(&state.pool, &next).await.unwrap());
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 101);
}

#[tokio::test]
async fn superseded_archived_or_expired_targets_end_without_creating_jobs() {
    for change in [
        "UPDATE mtls_rotations SET phase='complete',outcome='superseded_unpublished_target'",
        "UPDATE credentials SET archived=true WHERE kind='ca_identity'",
        "UPDATE mtls_ca_generations SET expires_at=clock_timestamp()-interval '1 second' WHERE ca_version=2",
    ] {
        let (state, ca, rotation) = fixture(1).await;
        let w = work(&state, &ca, &rotation, "owner").await;
        sqlx::query(change).execute(&state.pool).await.unwrap();
        assert!(!page(&state.pool, &w).await.unwrap());
        assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
        assert_eq!(
            count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
            0
        );
        assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE state='complete' AND error_code='rotation_trust_superseded'").await,1);
        assert_eq!(
            count(&state, "SELECT published_version FROM mtls_authorities").await,
            1
        );
    }
}

#[tokio::test]
async fn failed_operations_require_retry_but_waiting_confirmation_and_deleting_nodes_are_never_requeued()
 {
    let (state, ca, rotation) = fixture(4).await;
    let w = work(&state, &ca, &rotation, "first").await;
    page(&state.pool, &w).await.unwrap();
    sqlx::query("UPDATE jobs SET status=CASE WHEN node_id='member-0001' THEN 'waiting_confirmation' ELSE 'failed' END").execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id='member-0004'")
        .execute(&state.pool)
        .await
        .unwrap();
    let w = work(&state, &ca, &rotation, "no-retry").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 4);
    sqlx::query("UPDATE mtls_rotations SET retry_requested=true")
        .execute(&state.pool)
        .await
        .unwrap();
    let w = work(&state, &ca, &rotation, "retry").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 6);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE node_id='member-0001'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE node_id='member-0004'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE phase='trust' AND NOT retry_requested"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn exact_claim_does_not_consume_another_rotations_work_and_busy_ca_owner_is_preserved() {
    let (state, ca, rotation) = fixture(0).await;
    let w = work(&state, &ca, &rotation, "owner").await;
    assert!(
        maintenance::claim_id(&state.pool, KIND, "other", "missing-work")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        maintenance::claim_id(&state.pool, KIND, "other", &w.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!pass(&state, &rotation, &ca, 2).await.unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE state='running' AND owner='owner'"
        )
        .await,
        1
    );
    assert!(page(&state.pool, &w).await.unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE phase='identities'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
}

pub(in crate::credentials::rotations) async fn prove(state: &AppState, ca: &str, node: &str) {
    crate::credentials::maintenance::renew_probe_on(&state.pool, &state.secrets, node, ca, 2)
        .await
        .unwrap();
    let (credential,version):(String,i64)=sqlx::query_as("SELECT credential_id,credential_version FROM mtls_probe_versions WHERE node_id=$1 AND ca_version=2 ORDER BY credential_version DESC LIMIT 1").bind(node).fetch_one(&state.pool).await.unwrap();
    let job = format!("proof-job:{node}");
    sqlx::query("INSERT INTO jobs(id,kind,node_id,status,stage,available_at,created_at,updated_at) VALUES($1,'deploy',$2,'succeeded','complete',now(),now(),now())").bind(&job).bind(node).execute(&state.pool).await.unwrap();
    let activation = format!("proof:{node}");
    sqlx::query("INSERT INTO mtls_activations(id,node_id,job_id,generation,target_revision,phase,previous_state,target_digest,snapshot_enc,trust,probe_credential_id,probe_credential_version,created_at,updated_at) VALUES($1,$2,$3,1,1,'committed','ready','fixture-digest',$4,'[]',$5,$6,now(),now())")
        .bind(&activation).bind(node).bind(job).bind(state.secrets.encrypt("{}").unwrap()).bind(credential).bind(version).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE nodes SET mtls_activation_id=$2 WHERE id=$1")
        .bind(node)
        .bind(activation)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,deployed) VALUES($1,$2,2,true)",
    )
    .bind(node)
    .bind(ca)
    .execute(&state.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn a_late_member_behind_the_cursor_and_expired_proof_override_cached_ready_flags() {
    let (state, ca, rotation) = fixture(1).await;
    let proof_node = "00000000-0000-4000-8000-000000000001";
    sqlx::query("DELETE FROM mtls_node_trust WHERE node_id='member-0001'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE nodes SET id=$1,node_token_hash='proof-node-token-hash' WHERE id='member-0001'",
    )
    .bind(proof_node)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,desired,deployed) VALUES($1,$2,1,true,true)").bind(proof_node).bind(&ca).execute(&state.pool).await.unwrap();
    prove(&state, &ca, proof_node).await;
    let w = work(&state, &ca, &rotation, "first").await;
    assert!(page(&state.pool, &w).await.unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotation_nodes WHERE trust_prepared"
        )
        .await,
        1
    );
    nodes(&state, &ca, 1).await;
    sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision,trust_prepared) VALUES($1,'member-0001',1,true)").bind(&rotation).execute(&state.pool).await.unwrap();
    sqlx::query(
        "UPDATE background_work SET payload=jsonb_set(payload,'{cursor}','\"zzz\"') WHERE kind=$1",
    )
    .bind(KIND)
    .execute(&state.pool)
    .await
    .unwrap();
    let w = work(&state, &ca, &rotation, "late-member").await;
    assert!(!page(&state.pool, &w).await.unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE phase='trust'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
    // Even after removal of the late binding, a cached prepared flag cannot
    // hide expiry of the actual target-generation probe material.
    sqlx::query(
        "UPDATE mtls_node_trust SET desired=false,deployed=false WHERE node_id='member-0001'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE mtls_probe_versions SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    let w = work(&state, &ca, &rotation, "expired-proof").await;
    assert!(!page(&state.pool, &w).await.unwrap());
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_rotation_nodes WHERE node_id='00000000-0000-4000-8000-000000000001' AND NOT trust_prepared").await,1);
}

#[tokio::test]
async fn unconfirmed_remote_trust_remains_a_member_without_desired_or_deployed_flags() {
    let (state, ca, rotation) = fixture(1).await;
    sqlx::query("UPDATE mtls_node_trust SET desired=false,deployed=false")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET mtls_state='recovery_required'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,status,stage,available_at,created_at,updated_at) VALUES('uncertain-job','deploy','member-0001','failed','recovery',now(),now(),now())").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_activations(id,node_id,job_id,generation,target_revision,phase,previous_state,target_digest,snapshot_enc,trust,created_at,updated_at) VALUES('uncertain','member-0001','uncertain-job',1,1,'recovery_required','ready','uncertain-digest',$1,'[]',now(),now())").bind(state.secrets.encrypt("{}").unwrap()).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_activation_trust(activation_id,ca_id,ca_version) VALUES('uncertain',$1,1)").bind(&ca).execute(&state.pool).await.unwrap();
    let w = work(&state, &ca, &rotation, "owner").await;
    assert!(!page(&state.pool, &w).await.unwrap());
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_rotation_nodes WHERE node_id='member-0001' AND NOT trust_prepared AND job_id IS NULL").await,1);
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
}

#[tokio::test]
async fn obsolete_rotation_members_are_removed_in_bounded_pages() {
    let (state, ca, rotation) = fixture(301).await;
    sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision) SELECT $1,id,desired_revision FROM nodes").bind(&rotation).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE mtls_node_trust SET desired=false,deployed=false")
        .execute(&state.pool)
        .await
        .unwrap();
    for remaining in [201, 101, 1, 0] {
        let w = work(&state, &ca, &rotation, "owner").await;
        assert!(page(&state.pool, &w).await.unwrap());
        assert_eq!(
            count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
            remaining
        );
    }
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
}

async fn admission_work(state: &AppState) -> Work {
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN_KIND, json!({}))
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1")
        .bind(SCAN_KIND)
        .execute(&state.pool)
        .await
        .unwrap();
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN_KIND, json!({}))
        .await
        .unwrap();
    maintenance::claim(&state.pool, SCAN_KIND, "admission")
        .await
        .unwrap()
        .unwrap()
}
pub(in crate::credentials::rotations) async fn many_rotations(state: &AppState, ca: &str, n: i32) {
    sqlx::query("INSERT INTO credentials(id,name,kind,latest_version,created_at,updated_at) SELECT 'admit-ca-'||i,'CA','ca_identity',2,now(),now() FROM generate_series(1,$1)i").bind(n).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) SELECT c.id,v.version,v.payload_enc,v.metadata,now() FROM credentials c CROSS JOIN credential_versions v WHERE c.id LIKE 'admit-ca-%' AND v.credential_id=$1").bind(ca).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_authorities(credential_id,source,published_version) SELECT id,'generated',1 FROM credentials WHERE id LIKE 'admit-ca-%'").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at,published_at) SELECT c.id,g.ca_version,g.state,g.expires_at,g.published_at FROM credentials c CROSS JOIN mtls_ca_generations g WHERE c.id LIKE 'admit-ca-%' AND g.ca_id=$1").bind(ca).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO credential_batches(id,credential_id,version,created_at) SELECT 'admit-batch-'||i,'admit-ca-'||i,2,now() FROM generate_series(1,$1)i").bind(n).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_rotations(id,ca_id,ca_version,phase,batch_id,next_action_at,created_at,updated_at) SELECT 'admit-rotation-'||i,'admit-ca-'||i,2,'trust','admit-batch-'||i,now(),now(),now() FROM generate_series(1,$1)i").bind(n).execute(&state.pool).await.unwrap();
}

#[tokio::test]
async fn three_hundred_one_rotations_are_admitted_in_pages_skipping_busy_sources_and_keeping_live_work()
 {
    let (state, ca, rotation) = fixture(0).await;
    many_rotations(&state, &ca, 300).await;
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM mtls_rotations WHERE id=$1 FOR UPDATE")
        .bind(&rotation)
        .fetch_one(&mut *held)
        .await
        .unwrap();
    for expected in [100, 200, 300] {
        let scan = admission_work(&state).await;
        admit(&state.pool, &scan).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-trust'"
            )
            .await,
            expected
        );
    }
    let live = maintenance::claim(&state.pool, KIND, "live")
        .await
        .unwrap()
        .unwrap();
    let scan = admission_work(&state).await;
    admit(&state.pool, &scan).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE owner='live' AND state='running'"
        )
        .await,
        1
    );
    for lease in &live.leases {
        assert!(lease.renew(&state.pool).await.unwrap());
    }
    held.rollback().await.unwrap();
    let scan = admission_work(&state).await;
    admit(&state.pool, &scan).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-trust'"
        )
        .await,
        301
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}

#[tokio::test]
async fn admission_confirmation_failure_rolls_back_all_child_work_and_native_retry_is_atomic() {
    let (state, ca, _) = fixture(0).await;
    many_rotations(&state, &ca, 100).await;
    let scan = admission_work(&state).await;
    sqlx::raw_sql("CREATE FUNCTION reject_trust_admit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-trust-admission' AND NEW.state='complete' THEN RAISE EXCEPTION 'confirmation rejected'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_trust_admit BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_trust_admit();").execute(&state.pool).await.unwrap();
    assert!(admit(&state.pool, &scan).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-trust'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_trust_admit ON background_work; CREATE SEQUENCE trust_admit_attempts; CREATE FUNCTION retry_trust_admit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-trust-admission' AND NEW.state='complete' AND nextval('trust_admit_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_trust_admit BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_trust_admit();").execute(&state.pool).await.unwrap();
    admit(&state.pool, &scan).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-trust'"
        )
        .await,
        100
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM trust_admit_attempts").await,
        2
    );
}

#[tokio::test]
async fn concurrent_schedulers_finish_one_shared_ready_trust_work_without_publishing_the_ca() {
    let (state, _, _) = fixture(0).await;
    let (a, b) = tokio::join!(scheduled_pass(&state), scheduled_pass(&state));
    a.unwrap();
    b.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE phase='identities'"
        )
        .await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-trust' AND state='complete'").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
}

#[tokio::test]
async fn empty_rotation_admission_uses_the_deadline_index_without_scanning_future_work() {
    let (state, ca, _) = fixture(0).await;
    many_rotations(&state, &ca, 1000).await;
    sqlx::query("UPDATE mtls_rotations SET next_action_at=clock_timestamp()+interval '1 day'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("ANALYZE mtls_rotations; ANALYZE mtls_ca_generations; ANALYZE credentials; ANALYZE background_work;").execute(&state.pool).await.unwrap();
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE,FORMAT JSON) {ADMISSION}"
    )))
    .bind(now)
    .bind(PAGE)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(plan.to_string().contains("mtls_rotations_due"), "{plan}");
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
    assert_eq!(
        count(&state, "SELECT count(*) FROM background_work").await,
        0
    );
}
