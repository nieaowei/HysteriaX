use super::{store::*, *};
use crate::{db, security::SecretBox, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::Value;

async fn empty() -> AppState {
    let pool = db::test_pool_with_max_connections(5).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    AppState::new(pool, secrets)
}
async fn fixture() -> AppState {
    let state = empty().await;
    initialize(&state.pool, &state.secrets).await.unwrap();
    state
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn metadata_snapshot(state: &AppState) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('templates',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM subscription_templates t),'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY template_id,version) FROM subscription_template_versions v))").fetch_one(&state.pool).await.unwrap()
}
async fn new_template(state: &AppState, name: &str) -> String {
    let (_, Json(created)) = create(
        State(state.clone()),
        Json(Create {
            expected_revision: 0,
            name: name.into(),
            format: "mihomo".into(),
            document: None,
        }),
    )
    .await
    .unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    let _ = publish(
        State(state.clone()),
        Path(id.clone()),
        Json(Revision {
            expected_revision: 1,
        }),
    )
    .await
    .unwrap();
    id
}
fn default_patch(revision: i64) -> Patch {
    Patch {
        expected_revision: revision,
        name: None,
        is_default: Some(true),
        archived: None,
        replacement_id: None,
        replacement_revision: None,
    }
}
#[tokio::test]
async fn concurrent_initialization_creates_exactly_one_seed_set() {
    let state = empty().await;
    let (a, b) = tokio::join!(
        initialize(&state.pool, &state.secrets),
        initialize(&state.pool, &state.secrets)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM subscription_templates").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM subscription_template_versions"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM subscription_template_initialization"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn every_template_mutation_rolls_back_on_success_audit_failure() {
    let state = fixture().await;
    let before = metadata_snapshot(&state).await;
    sqlx::raw_sql("CREATE FUNCTION reject_template_success() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.entity_type='subscription_template' AND NEW.detail_json->>'result'='success' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_template_success BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_template_success();").execute(&state.pool).await.unwrap();
    assert!(
        create(
            State(state.clone()),
            Json(Create {
                expected_revision: 0,
                name: "New".into(),
                format: "mihomo".into(),
                document: None
            })
        )
        .await
        .is_err()
    );
    assert!(
        save(
            State(state.clone()),
            Path("default-mihomo".into()),
            Json(Save {
                expected_revision: 1,
                document: render::single_seed("mihomo")
            })
        )
        .await
        .is_err()
    );
    assert!(
        publish(
            State(state.clone()),
            Path("default-mihomo".into()),
            Json(Revision {
                expected_revision: 1
            })
        )
        .await
        .is_err()
    );
    assert!(
        rollback(
            State(state.clone()),
            Path("default-mihomo".into()),
            Json(Rollback {
                expected_revision: 1,
                version: 1
            })
        )
        .await
        .is_err()
    );
    let mut input = default_patch(1);
    input.is_default = None;
    input.name = Some("Changed".into());
    assert!(
        patch(
            State(state.clone()),
            Path("default-mihomo".into()),
            Json(input)
        )
        .await
        .is_err()
    );
    assert_eq!(metadata_snapshot(&state).await, before);
    assert_eq!(count(&state,"SELECT count(*) FROM audit_records WHERE entity_type='subscription_template' AND detail_json->>'result'='failure'").await,5);
}
#[tokio::test]
async fn publication_retries_keep_one_version_cipher_and_success_audit() {
    let state = fixture().await;
    sqlx::raw_sql("CREATE SEQUENCE template_attempts; CREATE FUNCTION retry_template_success() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='template.published' AND nextval('template_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_template_success BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_template_success();").execute(&state.pool).await.unwrap();
    let Json(receipt) = publish(
        State(state.clone()),
        Path("default-mihomo".into()),
        Json(Revision {
            expected_revision: 1,
        }),
    )
    .await
    .unwrap();
    assert_eq!(receipt["revision"], 2);
    assert_eq!(receipt["published_version"], 2);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM subscription_template_versions WHERE template_id='default-mihomo'"
        )
        .await,
        2
    );
    assert_eq!(count(&state,"SELECT count(DISTINCT document_enc) FROM subscription_template_versions WHERE template_id='default-mihomo'").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='template.published'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='template.publish_failed'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM template_attempts").await,
        2
    );
}
#[tokio::test]
async fn same_revision_publications_commit_only_one_new_version() {
    let state = fixture().await;
    let (a, b) = tokio::join!(
        publish(
            State(state.clone()),
            Path("default-mihomo".into()),
            Json(Revision {
                expected_revision: 1
            })
        ),
        publish(
            State(state.clone()),
            Path("default-mihomo".into()),
            Json(Revision {
                expected_revision: 1
            })
        )
    );
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
        count(
            &state,
            "SELECT count(*) FROM subscription_template_versions WHERE template_id='default-mihomo'"
        )
        .await,
        2
    );
}
#[tokio::test]
async fn competing_default_switches_keep_one_default_and_atomic_revisions() {
    let state = fixture().await;
    let a = new_template(&state, "First").await;
    let b = new_template(&state, "Second").await;
    let (first, second) = tokio::join!(
        patch(
            State(state.clone()),
            Path(a.clone()),
            Json(default_patch(2))
        ),
        patch(
            State(state.clone()),
            Path(b.clone()),
            Json(default_patch(2))
        )
    );
    let _ = first.unwrap();
    let _ = second.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM subscription_templates WHERE format='mihomo' AND is_default AND NOT archived").await,1);
    assert_eq!(count(&state,"SELECT count(*) FROM subscription_templates WHERE format='singbox' AND is_default AND revision=1").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='template.default_changed'"
        )
        .await,
        4
    );
}
#[tokio::test]
async fn metadata_edits_do_not_lock_unrelated_templates_of_the_same_format() {
    let state = fixture().await;
    let a = new_template(&state, "First").await;
    let b = new_template(&state, "Second").await;
    let mut blocker = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM subscription_templates WHERE id=$1 FOR UPDATE")
        .bind(a)
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let mut input = default_patch(2);
    input.is_default = None;
    input.name = Some("Updated".into());
    let Json(receipt) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        patch(State(state.clone()), Path(b), Json(input)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(receipt["revision"], 3);
    blocker.rollback().await.unwrap();
}
