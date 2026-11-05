use super::*;
use serde_json::json;

#[tokio::test]
async fn dns_jobs_serialize_by_zone_and_deployments_wait_for_dns_dependencies() {
    let (state, ssh, connection, zone) = crate::dns::tests::fixture().await;
    let node = crate::dns::tests::node(&state, &ssh).await;
    let mut tx = state.pool.begin().await.unwrap();
    let deploy = crate::api::enqueue_job_in_tx(&mut tx, "deploy", Some(&node), Some(1))
        .await
        .unwrap();
    let mut jobs = Vec::new();
    for index in 0..2 {
        let key = format!("zone-job:{index}");
        let receipt = crate::dns::enqueue(
            &mut tx,
            crate::dns::Operation {
                key: &key,
                request: &json!({"index":index}),
                connection: &connection,
                version: 1,
                resource_type: "dns_zone",
                resource: &zone,
                resource_name: "example.test",
                resource_key: format!("dns-zone:{zone}"),
                action: "zone-refresh",
                payload: json!({}),
                node: Some(&node),
            },
        )
        .await
        .unwrap();
        jobs.push(receipt["job_id"].as_str().unwrap().to_owned());
    }
    tx.commit().await.unwrap();
    let first = claim_next(&state.pool).await.unwrap().unwrap();
    assert_eq!(first.id, jobs[0]);
    assert!(claim_next(&state.pool).await.unwrap().is_none());
    sqlx::query("UPDATE jobs SET status='succeeded' WHERE id=$1")
        .bind(&first.id)
        .execute(&state.pool)
        .await
        .unwrap();
    let second = claim_next(&state.pool).await.unwrap().unwrap();
    assert_eq!(second.id, jobs[1]);
    sqlx::query("UPDATE jobs SET status='succeeded' WHERE id=$1")
        .bind(&second.id)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(claim_next(&state.pool).await.unwrap().unwrap().id, deploy);
}

#[tokio::test]
async fn successful_deployment_publishes_executed_connection_and_cancelled_result_cannot_publish() {
    let (state, ssh, _, _) = crate::dns::tests::fixture().await;
    let node = crate::dns::tests::node(&state, &ssh).await;
    let mut tx = state.pool.begin().await.unwrap();
    crate::api::enqueue_job_in_tx(&mut tx, "deploy", Some(&node), Some(1))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    let connection = json!({"public_host":"old.example.test","public_port":443,"listen_addr":":443","tls_sni":null,"tls_skip_verify":false});
    sqlx::query("UPDATE nodes SET public_host='concurrent.example.test' WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let output = || JobOutput {
        stage: "health_checked".into(),
        result: json!({}),
        node_state: Some("deployed".into()),
        deployed_revision: Some(1),
        deployed_config: Some(state.secrets.encrypt("{}").unwrap()),
        deployed_sha256: Some("digest".into()),
        published_connection: Some(connection.clone()),
        delete_node: false,
    };
    succeed(&state, &job, output()).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Option<serde_json::Value>>(
            "SELECT published_connection FROM nodes WHERE id=$1"
        )
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        Some(connection.clone())
    );
    let mut stale = output();
    stale.published_connection = Some(json!({"public_host":"stale.example.test"}));
    succeed(&state, &job, stale).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Option<serde_json::Value>>(
            "SELECT published_connection FROM nodes WHERE id=$1"
        )
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        Some(connection)
    );
}

#[tokio::test]
async fn successful_uninstall_queues_dns_cleanup_once_after_removing_node() {
    let (state, ssh, _, _) = crate::dns::tests::fixture().await;
    let (node, ids) = crate::dns::tests::allocated_node(&state, &ssh).await;
    sqlx::query("UPDATE nodes SET state='deleting',deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    crate::api::enqueue_job_in_tx(&mut tx, "uninstall", Some(&node), Some(1))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    assert_eq!(job.kind, "uninstall");
    for _ in 0..2 {
        succeed(
            &state,
            &job,
            JobOutput {
                stage: "uninstalled".into(),
                result: json!({}),
                node_state: None,
                deployed_revision: None,
                deployed_config: None,
                deployed_sha256: None,
                published_connection: None,
                delete_node: true,
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM jobs WHERE kind='dns-record-delete' AND status='queued' AND node_id IS NULL").fetch_one(&state.pool).await.unwrap(), ids.len() as i64);
    assert_eq!(
        claim_next(&state.pool).await.unwrap().unwrap().kind,
        "dns-record-delete"
    );
}
