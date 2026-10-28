use super::*;
use crate::{execution, state::AppState};

async fn fixture() -> (AppState, JobInput, Vec<execution::Execution>) {
    fixture_on(db::test_pool_with_max_connections(8).await).await
}
async fn seed_on(pool: PgPool) -> (AppState, JobInput) {
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT id,id,'127.0.0.1',22,'root','password','unused','example.test',443,':443',CASE WHEN id='a' THEN $1 ELSE id END,'unused','unused','{}',clock_timestamp(),clock_timestamp() FROM unnest(ARRAY['a','b'])id").bind(token_digest("node-auth")).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,attempts,available_at,created_at,updated_at) VALUES('one','deploy','a',1,'running','starting',1,clock_timestamp(),clock_timestamp(),clock_timestamp()),('two','deploy','b',1,'running','starting',1,clock_timestamp(),clock_timestamp(),clock_timestamp())").execute(&pool).await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&pool).await.unwrap();
    let state = AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    let job = JobInput {
        id: "one".into(),
        kind: "deploy".into(),
        node_id: Some("a".into()),
        target_revision: Some(1),
        payload: json!({}),
        attempts: 1,
    };
    (state, job)
}
async fn fixture_on(pool: PgPool) -> (AppState, JobInput, Vec<execution::Execution>) {
    let (state, job) = seed_on(pool).await;
    let mut tx = state.pool.begin().await.unwrap();
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
    (state, job, leases)
}

async fn auth(state: &AppState, token: &str) -> Value {
    serde_json::to_value(
        crate::api::subscriptions::hy2_auth(
            axum::extract::State(state.clone()),
            axum::extract::Path(("a".into(), "node-auth".into())),
            axum::Json(
                serde_json::from_value(json!({"addr":"127.0.0.1","auth":token,"tx":0})).unwrap(),
            ),
        )
        .await
        .unwrap()
        .0,
    )
    .unwrap()
}
async fn row(pool: &PgPool) -> Value {
    sqlx::query_scalar("SELECT coalesce((SELECT to_jsonb(p) FROM deployment_probe_tokens p WHERE node_id='a'),'null'::jsonb)").fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn leased_token_uses_database_time_and_authenticates_a_unique_identity_without_the_old_lock()
{
    let (state, job, leases) = fixture().await;
    let hash = token_digest("first");
    let legacy = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='b' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        execution::supervise(
            state.pool.clone(),
            leases.clone(),
            install(&state.pool, &job, &hash),
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(
        auth(&state, "first").await,
        json!({"ok":true,"id":identity("a",&hash)})
    );
    assert_ne!(identity("a", &hash), identity("a", &token_digest("second")));
    assert!(sqlx::query_scalar::<_,bool>("SELECT owner='owner' AND node_generation=1 AND job_generation=1 AND expires_at>clock_timestamp()+interval '14 minutes' AND expires_at<=clock_timestamp()+interval '15 minutes' FROM deployment_probe_tokens WHERE node_id='a'").fetch_one(&state.pool).await.unwrap());
    execution::supervise(state.pool.clone(), leases, remove(&state.pool, &job, &hash))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row(&state.pool).await, Value::Null);
    held.rollback().await.unwrap();
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn token_authentication_rejects_ended_expired_blocked_or_replaced_execution_incarnations() {
    let (state, job, leases) = fixture().await;
    let hash = token_digest("first");
    execution::supervise(
        state.pool.clone(),
        leases,
        install(&state.pool, &job, &hash),
    )
    .await
    .unwrap()
    .unwrap();
    for sql in [
        "UPDATE jobs SET status='cancelled' WHERE id='one'",
        "UPDATE resource_leases SET blocked=true WHERE resource_key='node:a'",
        "UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key='job:one'",
        "UPDATE resource_leases SET generation=2 WHERE resource_key='node:a'",
        "UPDATE resource_leases SET generation=2 WHERE resource_key='job:one'",
        "UPDATE resource_leases SET owner='other' WHERE resource_key='node:a'",
        "UPDATE deployment_probe_tokens SET expires_at=clock_timestamp()-interval '1 second' WHERE node_id='a'",
    ] {
        sqlx::query(sql).execute(&state.pool).await.unwrap();
        assert_eq!(auth(&state, "first").await["ok"], false, "{sql}");
        sqlx::raw_sql("UPDATE jobs SET status='running' WHERE id='one'; UPDATE resource_leases SET blocked=false,owner='owner',generation=1,expires_at=clock_timestamp()+interval '1 minute'; UPDATE deployment_probe_tokens SET expires_at=clock_timestamp()+interval '15 minutes'").execute(&state.pool).await.unwrap();
        assert_eq!(auth(&state, "first").await["ok"], true);
    }
}

#[tokio::test]
async fn native_token_retry_is_atomic_and_a_rejected_insert_preserves_the_current_credential() {
    let (state, job, leases) = fixture().await;
    let first = token_digest("first");
    execution::supervise(
        state.pool.clone(),
        leases.clone(),
        install(&state.pool, &job, &first),
    )
    .await
    .unwrap()
    .unwrap();
    let before = row(&state.pool).await;
    sqlx::raw_sql("CREATE FUNCTION reject_probe_token() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject'; END $$; CREATE TRIGGER reject_probe_token BEFORE INSERT ON deployment_probe_tokens FOR EACH ROW EXECUTE FUNCTION reject_probe_token();").execute(&state.pool).await.unwrap();
    assert!(
        execution::supervise(
            state.pool.clone(),
            leases.clone(),
            install(&state.pool, &job, &token_digest("second"))
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, row(&state.pool).await);
    sqlx::raw_sql("DROP TRIGGER reject_probe_token ON deployment_probe_tokens; CREATE SEQUENCE probe_token_attempts; CREATE FUNCTION retry_probe_token() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('probe_token_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_probe_token BEFORE INSERT ON deployment_probe_tokens FOR EACH ROW EXECUTE FUNCTION retry_probe_token();").execute(&state.pool).await.unwrap();
    execution::supervise(
        state.pool.clone(),
        leases,
        install(&state.pool, &job, &token_digest("second")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM probe_token_attempts")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(auth(&state, "first").await["ok"], false);
    assert_eq!(auth(&state, "second").await["ok"], true);
}

#[tokio::test]
async fn takeover_changes_directories_and_late_install_or_cleanup_cannot_touch_the_new_token() {
    let (state, job, leases) = fixture().await;
    let first = token_digest("first");
    let old_paths = execution::supervise(state.pool.clone(), leases.clone(), async {
        paths(&job, &first)
    })
    .await
    .unwrap()
    .unwrap();
    execution::supervise(
        state.pool.clone(),
        leases.clone(),
        install(&state.pool, &job, &first),
    )
    .await
    .unwrap()
    .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
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
    assert_eq!(auth(&state, "first").await["ok"], false);
    let new_paths = execution::supervise(state.pool.clone(), next.clone(), async {
        paths(&job, &first)
    })
    .await
    .unwrap()
    .unwrap();
    assert_ne!(old_paths.stage, new_paths.stage);
    assert_ne!(old_paths.probe, new_paths.probe);
    execution::supervise(
        state.pool.clone(),
        next,
        install(&state.pool, &job, &token_digest("second")),
    )
    .await
    .unwrap()
    .unwrap();
    let before = row(&state.pool).await;
    assert!(
        execution::supervise(
            state.pool.clone(),
            leases.clone(),
            install(&state.pool, &job, &first)
        )
        .await
        .is_none()
    );
    assert!(
        execution::supervise(
            state.pool.clone(),
            leases,
            remove(&state.pool, &job, &first)
        )
        .await
        .is_none()
    );
    assert_eq!(before, row(&state.pool).await);
    assert_eq!(auth(&state, "second").await["ok"], true);
}

#[tokio::test]
async fn mismatched_job_or_missing_node_scope_cannot_install_or_remove_a_credential() {
    let (state, job, leases) = fixture().await;
    let first = token_digest("first");
    execution::supervise(
        state.pool.clone(),
        leases.clone(),
        install(&state.pool, &job, &first),
    )
    .await
    .unwrap()
    .unwrap();
    let before = row(&state.pool).await;
    let mut wrong = job.clone();
    wrong.id = "two".into();
    wrong.node_id = Some("b".into());
    assert!(
        execution::supervise(
            state.pool.clone(),
            leases.clone(),
            install(&state.pool, &wrong, &first)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(
        execution::supervise(
            state.pool.clone(),
            vec![leases[0].clone()],
            remove(&state.pool, &job, &first)
        )
        .await
        .unwrap()
        .is_err()
    );
    let mut wrong = job.clone();
    wrong.attempts = 2;
    assert!(
        execution::supervise(
            state.pool.clone(),
            leases,
            install(&state.pool, &wrong, &first)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, row(&state.pool).await);
}

#[test]
fn directory_components_are_opaque_and_separate_stage_probe_and_fixture_invocations() {
    let job = JobInput {
        id: "job ; $(exit 1)".into(),
        kind: "deploy".into(),
        node_id: Some("../../node 'unsafe'".into()),
        target_revision: Some(1),
        payload: json!({}),
        attempts: 1,
    };
    let one = paths(&job, "one").unwrap();
    let two = paths(&job, "two").unwrap();
    assert_ne!(one.stage, one.probe);
    assert_ne!(one.stage, two.stage);
    for (path, prefix) in [
        (&one.stage, "/tmp/hysteriax-stage-"),
        (&one.probe, "/tmp/hysteriax-probe-"),
    ] {
        let key = path.strip_prefix(prefix).unwrap();
        assert_eq!(key.len(), 64);
        assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
    }
}

#[test]
fn cleanup_checks_the_configuration_path_before_signalling_a_pid_and_removes_only_its_directory() {
    for matches in [false, true] {
        let root = std::env::temp_dir().join(format!("hx-probe-cleanup-{}", uuid::Uuid::new_v4()));
        let owned = root.join("owned");
        let other = root.join("other");
        std::fs::create_dir_all(&owned).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("keep"), "peer").unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let proc = root.join("proc").join(pid.to_string());
        std::fs::create_dir_all(&proc).unwrap();
        let config = if matches {
            owned.join("client.yaml")
        } else {
            other.join("client.yaml")
        };
        let args = format!(
            "timeout\0--kill-after=2s\090s\0/opt/hysteriax/hysteria\0client\0-c\0{}\0",
            config.display()
        );
        std::fs::write(proc.join("cmdline"), args).unwrap();
        let script = super::super::probe_cleanup_command(owned.to_str().unwrap(), Some(pid))
            .replace("/proc/", &format!("{}/proc/", root.display()));
        let result = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .status()
            .unwrap();
        let exited = child.try_wait().unwrap().is_some();
        if !exited {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        assert!(result.success());
        assert_eq!(exited, matches);
        assert!(!owned.exists());
        assert_eq!(std::fs::read_to_string(other.join("keep")).unwrap(), "peer");
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn restored_counter_collision_cannot_authenticate_or_clean_up_another_epochs_probe() {
    let (state, job, old) = fixture().await;
    let hash = token_digest("old");
    let previous = execution::supervise(state.pool.clone(), old.clone(), async {
        install(&state.pool, &job, &hash).await.unwrap();
        paths(&job, &hash).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(auth(&state, "old").await["ok"], true);
    let epoch = uuid::Uuid::new_v4().to_string();
    sqlx::query("UPDATE cache_version_epoch SET epoch=$1 WHERE id")
        .bind(&epoch)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(auth(&state, "old").await["ok"], false);
    // Match all prior counters and ownership fields, with live new-epoch leases.
    sqlx::query("UPDATE resource_leases SET epoch=$1 WHERE work_id='one'")
        .bind(&epoch)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(auth(&state, "old").await["ok"], false);
    let mut current = old.clone();
    for lease in &mut current {
        lease.epoch = epoch.clone();
    }
    let next = execution::supervise(state.pool.clone(), current.clone(), async {
        paths(&job, &hash).unwrap()
    })
    .await
    .unwrap();
    assert_ne!(previous.stage, next.stage);
    assert_ne!(previous.probe, next.probe);
    execution::supervise(
        state.pool.clone(),
        current.clone(),
        install(&state.pool, &job, &token_digest("fresh")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(auth(&state, "fresh").await["ok"], true);
    let before = row(&state.pool).await;
    assert!(
        execution::supervise(
            state.pool.clone(),
            old,
            remove(&state.pool, &job, &token_digest("fresh"))
        )
        .await
        .is_none()
    );
    assert_eq!(before, row(&state.pool).await);
    execution::supervise(
        state.pool.clone(),
        current,
        remove(&state.pool, &job, &token_digest("fresh")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(auth(&state, "fresh").await["ok"], false);
}
