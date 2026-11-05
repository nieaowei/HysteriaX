use super::*;

async fn fixture() -> AppState {
    use base64::Engine;
    let pool = crate::db::test_pool().await;
    let key = base64::engine::general_purpose::STANDARD_NO_PAD.encode([7_u8; 32]);
    let state = AppState::new(pool, crate::security::SecretBox::from_base64(&key).unwrap());
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at,node_name,resource_name,error_message) SELECT 'job-' || lpad(i::text, 3, '0'), 'sync', 'failed', 'failed', now(), '2026-01-01'::timestamptz, now(), CASE WHEN i=1 THEN '历史节点' ELSE 'Node' END, CASE WHEN i=2 THEN 'example%_record' ELSE NULL END, CASE WHEN i=3 THEN 'old failure' ELSE NULL END FROM generate_series(1,235) i")
        .execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE jobs SET retry_of_job_id='job-001' WHERE id='job-002'")
        .execute(&state.pool)
        .await
        .unwrap();
    state
}

async fn page(state: &AppState, value: Value) -> JobsPage {
    list_jobs(
        State(state.clone()),
        Query(serde_json::from_value(value).unwrap()),
    )
    .await
    .unwrap()
    .0
}

#[tokio::test]
async fn jobs_pagination_covers_history_with_stable_ties_and_clamps_pages() {
    let state = fixture().await;
    let first = page(&state, json!({})).await;
    assert_eq!(
        (first.total, first.page, first.page_size, first.items.len()),
        (235, 1, 50, 50)
    );
    assert_eq!(first.items[0].id, "job-235");
    let mut ids = Vec::new();
    for n in 1..=5 {
        let result = page(&state, json!({"page": n})).await;
        ids.extend(result.items.into_iter().map(|job| job.id));
    }
    assert_eq!(ids.len(), 235);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        235
    );
    assert_eq!(ids.last().unwrap(), "job-001");
    let last = page(&state, json!({"page": i64::MAX})).await;
    assert_eq!((last.page, last.items.len()), (5, 35));
    let oldest = page(&state, json!({"order":"asc", "page_size":25})).await;
    assert_eq!(oldest.items[0].id, "job-001");
    assert_eq!(oldest.items[0].retry_job_id.as_deref(), Some("job-002"));
    assert_eq!(oldest.items[1].retry_of_job_id.as_deref(), Some("job-001"));
}

#[tokio::test]
async fn jobs_pagination_searches_all_history_and_treats_wildcards_literally() {
    let state = fixture().await;
    for search in [" 历史节点 ", "JOB-001", "%_", "OLD FAILURE"] {
        let result = page(&state, json!({"q":search, "page":200})).await;
        assert_eq!((result.total, result.page, result.items.len()), (1, 1, 1));
    }
    let empty = page(&state, json!({"q":"' OR true --"})).await;
    assert_eq!((empty.total, empty.page, empty.items.len()), (0, 1, 0));
    for sort in ["node", "kind", "stage", "status", "created_at"] {
        let result = page(&state, json!({"sort":sort, "order":"asc"})).await;
        assert_eq!(result.total, 235);
    }
    sqlx::query("DELETE FROM jobs")
        .execute(&state.pool)
        .await
        .unwrap();
    let empty = page(&state, json!({"page":2})).await;
    assert_eq!((empty.total, empty.page, empty.items.len()), (0, 1, 0));
}

#[test]
fn jobs_pagination_rejects_invalid_parameters() {
    for value in [
        json!({"page":0}),
        json!({"page":-1}),
        json!({"page_size":0}),
        json!({"page_size":201}),
        json!({"sort":"id; DROP TABLE jobs"}),
        json!({"order":"DESC NULLS FIRST"}),
    ] {
        let query: JobsQuery = serde_json::from_value(value).unwrap();
        assert!(query.validate().is_err());
    }
}
