use super::*;

async fn fixture() -> (AppState, String, String) {
    let (state, _, connection, zone) = crate::dns::tests::fixture().await;
    sqlx::query("INSERT INTO dns_connections(id,name,provider,credential_id,credential_version) SELECT 'connection-two','Two',provider,credential_id,credential_version FROM dns_connections WHERE id=$1")
        .bind(&connection).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO dns_zones(id,connection_id,provider_zone_id,name,enabled) VALUES('zone-two','connection-two','remote-two','other.test',true),('zone-three',$1,'remote-three','third.test',false)")
        .bind(&connection).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO dns_records(id,zone_id,name,record_type,content,ttl,origin,state,revision,resolution_status) SELECT 'record-'||lpad(i::text,3,'0'),CASE WHEN i<=200 THEN 'zone' ELSE 'zone-two' END,'same.example.test',CASE WHEN i%2=0 THEN 'AAAA' ELSE 'A' END,CASE WHEN i=235 THEN '历史%_目标' ELSE '8.8.8.8' END,300,'hysteriax','synced',i,'ok' FROM generate_series(1,235) i")
        .execute(&state.pool).await.unwrap();
    (state, connection, zone)
}

async fn page(state: &AppState, query: Value) -> Value {
    list_records_page(
        State(state.clone()),
        Query(serde_json::from_value(query).unwrap()),
    )
    .await
    .unwrap()
    .0
}

#[tokio::test]
async fn dns_pagination_covers_all_records_with_stable_sorting_and_filters() {
    let (state, connection, zone) = fixture().await;
    let first = page(&state, json!({})).await;
    assert_eq!(first["total"], 235);
    assert_eq!(first["page_size"], 50);
    let mut ids = Vec::new();
    for n in 1..=5 {
        let result = page(&state, json!({"page":n})).await;
        ids.extend(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["id"].as_str().unwrap().to_owned()),
        );
    }
    assert_eq!(ids.len(), 235);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        235
    );
    let last = page(&state, json!({"page":i64::MAX})).await;
    assert_eq!(last["page"], 5);
    assert_eq!(last["items"].as_array().unwrap().len(), 35);
    assert_eq!(page(&state, json!({"zone_id":zone})).await["total"], 200);
    assert_eq!(
        page(&state, json!({"connection_id":connection})).await["total"],
        200
    );
    assert_eq!(
        page(
            &state,
            json!({"connection_id":connection,"zone_id":"zone-two"})
        )
        .await["total"],
        0
    );
    assert_eq!(
        page(&state, json!({"zone_id":"zone-two","page":99})).await["page"],
        1
    );
    assert_eq!(
        page(&state, json!({"zone_id":"","connection_id":""})).await["total"],
        235
    );
    let reverse = page(&state, json!({"order":"desc","page_size":25})).await;
    let reversed: Vec<_> = ids.iter().rev().take(25).map(String::as_str).collect();
    assert_eq!(
        reverse["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        reversed
    );
    assert_eq!(
        page(&state, json!({"sort":"content","order":"desc"})).await["items"][0]["id"],
        "record-235"
    );
    let Json(catalog) = list_records(State(state.clone()), Query(RecordQuery::default()))
        .await
        .unwrap();
    assert_eq!(catalog.len(), 235);
}

#[tokio::test]
async fn dns_pagination_preserves_bound_deleted_records_metadata_and_literal_search() {
    let (state, _, _) = fixture().await;
    let ssh: String =
        sqlx::query_scalar("SELECT id FROM credentials WHERE kind='ssh_password' LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let (_, Json(created)) = crate::api::nodes::create(
        State(state.clone()),
        Json(crate::dns::tests::node_request(&ssh, None)),
    )
    .await
    .unwrap();
    let node = created["node"]["id"].as_str().unwrap().to_owned();
    sqlx::query("INSERT INTO dns_bindings(node_id,zone_id,hostname,record_ids) VALUES($1,'zone','same.example.test','[\"record-001\"]')")
        .bind(&node).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE dns_records SET deleted_at=now(),state='deleted' WHERE id IN ('record-001','record-002')")
        .execute(&state.pool).await.unwrap();
    assert_eq!(page(&state, json!({})).await["total"], 234);
    let bound = page(&state, json!({"q":"record-001"})).await;
    assert_eq!(bound["total"], 1);
    let record = &bound["items"][0];
    assert_eq!(record["bound_node_id"], node);
    assert_eq!(record["state"], "deleted");
    assert_eq!(record["origin"], "hysteriax");
    assert_eq!(record["ttl"], 300);
    let Json(detail) = get_record(State(state.clone()), Path("record-001".into()))
        .await
        .unwrap();
    assert_eq!(record, &detail);
    assert_eq!(page(&state, json!({"q":"record-002"})).await["total"], 0);
    for search in [" 历史%_ ", "RECORD-235"] {
        let result = page(&state, json!({"q":search})).await;
        assert_eq!(result["total"], 1);
        assert_eq!(result["items"][0]["id"], "record-235");
    }
    let empty = page(&state, json!({"q":"' OR true --","page":100})).await;
    assert_eq!(empty["total"], 0);
    assert_eq!(empty["page"], 1);
    assert_eq!(empty["items"], json!([]));
    sqlx::query("DELETE FROM dns_bindings")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM dns_records")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(page(&state, json!({"page":2})).await["page"], 1);
}

#[test]
fn dns_pagination_rejects_invalid_parameters() {
    for value in [
        json!({"page":0}),
        json!({"page":-1}),
        json!({"page_size":0}),
        json!({"page_size":201}),
        json!({"sort":"name;--"}),
        json!({"order":"ASC;--"}),
    ] {
        let query: RecordsPageQuery = serde_json::from_value(value).unwrap();
        assert!(query.validate().is_err());
    }
}
