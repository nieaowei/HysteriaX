use super::*;

async fn fixture() -> AppState {
    use base64::Engine;
    let pool = crate::db::test_pool().await;
    let key = base64::engine::general_purpose::STANDARD_NO_PAD.encode([8_u8; 32]);
    let state = AppState::new(pool, crate::security::SecretBox::from_base64(&key).unwrap());
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) SELECT 'audit-' || lpad(i::text,3,'0'), CASE WHEN i=2 THEN 'system' ELSE 'admin' END, CASE WHEN i=1 THEN 'user.deleted' ELSE 'node.created' END, CASE WHEN i=1 THEN 'user' ELSE 'node' END, CASE WHEN i=1 THEN '历史%_对象' ELSE lpad(i::text,3,'0') END, jsonb_build_object('kept',i), '2026-01-01'::timestamptz FROM generate_series(1,537) i")
        .execute(&state.pool).await.unwrap();
    state
}

async fn page(state: &AppState, query: Value) -> Value {
    list_audit_records(
        State(state.clone()),
        Query(serde_json::from_value(query).unwrap()),
    )
    .await
    .unwrap()
    .0
}

#[tokio::test]
async fn audit_pagination_covers_all_history_and_preserves_fields() {
    let state = fixture().await;
    let first = page(&state, json!({})).await;
    assert_eq!(first["total"], 537);
    assert_eq!(first["page_size"], 50);
    assert_eq!(first["items"][0]["id"], "audit-537");
    let mut ids = Vec::new();
    for n in 1..=11 {
        let result = page(&state, json!({"page": n})).await;
        ids.extend(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["id"].as_str().unwrap().to_owned()),
        );
    }
    assert_eq!(ids.len(), 537);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        537
    );
    assert_eq!(ids.last().unwrap(), "audit-001");
    let last = page(&state, json!({"page":i64::MAX})).await;
    assert_eq!(last["page"], 11);
    assert_eq!(last["items"].as_array().unwrap().len(), 37);
    let oldest = page(&state, json!({"order":"asc","page_size":25})).await;
    assert_eq!(oldest["items"][0]["id"], "audit-001");
    assert_eq!(oldest["items"][0]["detail"]["kept"], 1);
    assert_eq!(oldest["items"][0]["actor"], "admin");
    assert_eq!(oldest["items"][0]["action"], "user.deleted");
    assert_eq!(oldest["items"][0]["entity_type"], "user");
    assert_eq!(oldest["items"][0]["entity_id"], "历史%_对象");
}

#[tokio::test]
async fn audit_pagination_searches_old_records_and_localized_label_matches() {
    let state = fixture().await;
    for query in [
        json!({"q":" 历史%_ "}),
        json!({"q":"AUDIT-001"}),
        json!({"q":"删除用户","action_matches":"user.deleted"}),
        json!({"q":"用户","entity_type_matches":"user"}),
        json!({"q":"系统","actor_matches":"system"}),
    ] {
        let result = page(&state, query).await;
        assert_eq!(result["total"], 1);
    }
    let combined = page(
        &state,
        json!({"q":"no-such-value", "action_matches":"user.deleted", "actor_matches":"system"}),
    )
    .await;
    assert_eq!(combined["total"], 2);
    let literal = page(
        &state,
        json!({"q":"' OR true --", "action_matches":"user.deleted'); DROP TABLE audit_records;--"}),
    )
    .await;
    assert_eq!(literal["total"], 0);
    assert_eq!(literal["page"], 1);
    for sort in ["action", "entity_type", "entity_id", "actor", "created_at"] {
        let result = page(&state, json!({"sort":sort,"order":"asc"})).await;
        assert_eq!(result["total"], 537);
    }
    let actors = page(&state, json!({"sort":"actor","order":"desc","page_size":1})).await;
    assert_eq!(actors["items"][0]["actor"], "system");
    sqlx::query("DELETE FROM audit_records")
        .execute(&state.pool)
        .await
        .unwrap();
    let empty = page(&state, json!({"page":10})).await;
    assert_eq!(empty["page"], 1);
    assert_eq!(empty["total"], 0);
    assert_eq!(empty["items"], json!([]));
}

#[test]
fn audit_pagination_rejects_invalid_parameters() {
    for value in [
        json!({"page":0}),
        json!({"page":-1}),
        json!({"page_size":0}),
        json!({"page_size":201}),
        json!({"sort":"detail_json; --"}),
        json!({"order":"DESC; --"}),
    ] {
        let query: AuditQuery = serde_json::from_value(value).unwrap();
        assert!(query.validate().is_err());
    }
}
