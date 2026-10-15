use super::tests::{allocated_node, fixture};
use crate::api;
use axum::{
    Json,
    extract::{Path, State},
};
use serde_json::{Value, json};
use sqlx::Row;

async fn patch(
    state: &crate::state::AppState,
    node: &str,
    revision: i64,
    fields: Value,
) -> Result<Value, crate::error::ApiError> {
    let mut request = fields;
    request["expected_revision"] = json!(revision);
    Ok(api::nodes::patch(
        State(state.clone()),
        Path(node.into()),
        Json(serde_json::from_value(request).unwrap()),
    )
    .await?
    .0)
}

#[tokio::test]
async fn saving_ssh_ip_queues_bound_dns_update_for_its_address_family() {
    for (ip, kind, old) in [
        ("9.9.9.9", "A", "8.8.8.8"),
        ("2606:4700:4700::1001", "AAAA", "2606:4700:4700::1111"),
    ] {
        let (state, ssh, _, _) = fixture().await;
        let (node, _) = allocated_node(&state, &ssh).await;
        let response = patch(&state, &node, 1, json!({"ssh_host":ip}))
            .await
            .unwrap();
        assert_eq!(response["dns_job_ids"].as_array().unwrap().len(), 1);
        let rows = sqlx::query("SELECT r.*,j.node_id,j.resource_key,o.payload,o.credential_version FROM dns_records r JOIN dns_operations o ON o.resource_id=r.id JOIN jobs j ON j.id=o.job_id WHERE j.kind='dns-record-update'").fetch_all(&state.pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.get::<String, _>("record_type"), kind);
        assert_eq!(
            row.get::<String, _>("content"),
            old,
            "provider result is not fabricated at enqueue time"
        );
        assert_eq!(
            row.get::<Option<String>, _>("node_id").as_deref(),
            Some(node.as_str())
        );
        assert_eq!(row.get::<String, _>("resource_key"), "dns-zone:zone");
        let payload: Value = row.get("payload");
        assert_eq!(payload["desired"]["content"], ip);
        assert_eq!(payload["desired"]["type"], kind);
        assert_eq!(payload["desired"]["ttl"], 1);
        assert_eq!(payload["desired"]["proxied"], false);
        assert_eq!(payload["baseline"]["content"], old);
        assert_eq!(row.get::<String, _>("state"), "pending");
        assert_eq!(row.get::<String, _>("resolution_status"), "unchecked");
        let other: i64 = sqlx::query_scalar("SELECT count(*) FROM dns_records WHERE record_type<>$1 AND desired IS NULL AND state='synced'").bind(kind).fetch_one(&state.pool).await.unwrap();
        assert_eq!(other, 1);
        let stale = patch(&state, &node, 1, json!({"ssh_host":"1.1.1.1"}))
            .await
            .unwrap_err();
        assert_eq!(stale.status, axum::http::StatusCode::CONFLICT);
    }
}

#[tokio::test]
async fn unchanged_ssh_save_repairs_old_dns_but_other_fields_and_nonpublic_hosts_do_not() {
    let (state, ssh, _, _) = fixture().await;
    let (node, _) = allocated_node(&state, &ssh).await;
    sqlx::query("UPDATE nodes SET ssh_host='9.9.9.9' WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    patch(&state, &node, 1, json!({"name":"Renamed"}))
        .await
        .unwrap();
    for (revision, host) in [
        (2, "10.0.0.1"),
        (3, "server.example.test"),
        (4, "127.0.0.1"),
    ] {
        let response = patch(&state, &node, revision, json!({"ssh_host":host}))
            .await
            .unwrap();
        assert_eq!(response["dns_job_ids"], json!([]));
    }
    sqlx::query("UPDATE nodes SET ssh_host='9.9.9.9' WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let response = patch(&state, &node, 5, json!({"ssh_host":" 9.9.9.9 "}))
        .await
        .unwrap();
    assert_eq!(response["dns_job_ids"].as_array().unwrap().len(), 1);
    // Simulate the provider acknowledgement; an already-correct save is a no-op.
    sqlx::query("UPDATE dns_records SET content=desired->>'content',remote_snapshot=desired,desired=NULL,state='synced' WHERE desired IS NOT NULL").execute(&state.pool).await.unwrap();
    let response = patch(&state, &node, 6, json!({"ssh_host":"9.9.9.9"}))
        .await
        .unwrap();
    assert_eq!(response["dns_job_ids"], json!([]));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='dns-record-update'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn pending_dns_and_enqueue_failures_roll_back_the_entire_node_edit() {
    let (state, ssh, _, _) = fixture().await;
    let (node, ids) = allocated_node(&state, &ssh).await;
    sqlx::query("UPDATE dns_records SET desired=jsonb_build_object('content','1.1.1.1'),state='pending' WHERE id=$1").bind(&ids[0]).execute(&state.pool).await.unwrap();
    let error = patch(&state, &node, 1, json!({"ssh_host":"9.9.9.9"}))
        .await
        .unwrap_err();
    assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
    let saved: (String, i64) =
        sqlx::query_as("SELECT ssh_host,desired_revision FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(saved, ("127.0.0.1".into(), 1));
    sqlx::query("UPDATE dns_records SET desired=NULL,state='synced' WHERE id=$1")
        .bind(&ids[0])
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION fail_ssh_dns_write() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='record-update' THEN RAISE EXCEPTION 'injected DNS enqueue failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_dns BEFORE INSERT ON dns_operations FOR EACH ROW EXECUTE FUNCTION fail_ssh_dns_write();").execute(&state.pool).await.unwrap();
    assert!(
        patch(&state, &node, 1, json!({"ssh_host":"9.9.9.9"}))
            .await
            .is_err()
    );
    let saved: (String, i64) =
        sqlx::query_as("SELECT ssh_host,desired_revision FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(saved, ("127.0.0.1".into(), 1));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='dns-record-update'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM config_versions WHERE node_id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM dns_records WHERE desired IS NOT NULL")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn unbound_cname_and_unmatched_address_family_are_preserved() {
    let (state, ssh, _, _) = fixture().await;
    let (node, ids) = allocated_node(&state, &ssh).await;
    sqlx::query("DELETE FROM dns_bindings WHERE node_id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        patch(&state, &node, 1, json!({"ssh_host":"9.9.9.9"}))
            .await
            .unwrap()["dns_job_ids"],
        json!([])
    );
    let hostname: String = sqlx::query_scalar("SELECT public_host FROM nodes WHERE id=$1")
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO dns_bindings(node_id,zone_id,hostname,record_ids) VALUES($1,'zone',$2,$3)",
    )
    .bind(&node)
    .bind(&hostname)
    .bind(json!([ids[0]]))
    .execute(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        patch(&state, &node, 2, json!({"ssh_host":"2606:4700:4700::1001"}))
            .await
            .unwrap()["dns_job_ids"],
        json!([])
    );
    sqlx::query(
        "UPDATE dns_records SET record_type='CNAME',content='alias.example.test' WHERE id=$1",
    )
    .bind(&ids[0])
    .execute(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        patch(&state, &node, 3, json!({"ssh_host":"1.1.1.1"}))
            .await
            .unwrap()["dns_job_ids"],
        json!([])
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='dns-record-update'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}
