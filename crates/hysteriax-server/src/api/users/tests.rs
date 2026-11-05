use axum::{Json, extract::Path};
use serde_json::Value;

use crate::{security::SecretBox, state::AppState};

use super::{RevisionRequest, reset_quota};

#[tokio::test]
async fn quota_reset_starts_a_new_usage_period_and_checks_revision() {
    let pool = crate::db::test_pool().await;
    sqlx::query("INSERT INTO users (id, name, enabled, quota_bytes, usage_bytes, revision, access_kick_enqueued_at, created_at, updated_at) VALUES ('user-id', 'Quota user', TRUE, 1000, 750, 2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();

    let state = AppState::new(
        pool.clone(),
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    let Json(response): Json<Value> = reset_quota(
        axum::extract::State(state.clone()),
        Path("user-id".to_owned()),
        Json(RevisionRequest {
            expected_revision: 2,
        }),
    )
    .await
    .unwrap();
    assert_eq!(response["usage_bytes"], 0);
    assert_eq!(response["revision"], 3);

    let (usage, revision, marker): (i64, i64, Option<String>) = sqlx::query_as(
        "SELECT usage_bytes, revision, access_kick_enqueued_at FROM users WHERE id = 'user-id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((usage, revision, marker), (0, 3, None));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM audit_records WHERE action = 'user.quota_reset'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert!(
        reset_quota(
            axum::extract::State(state),
            Path("user-id".to_owned()),
            Json(RevisionRequest {
                expected_revision: 2,
            }),
        )
        .await
        .is_err()
    );
}
