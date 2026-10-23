use super::super::tests::{client, count, serve, snapshot};
use super::*;
use reqwest::Client;

struct Case {
    method: Method,
    path: String,
    body: Value,
    status: StatusCode,
    node: Option<String>,
}
async fn fixture(kind: usize) -> (AppState, Case) {
    let (state, ssh, _, _) = crate::dns::tests::fixture().await;
    crate::credentials::reference_index::initialize(&state.pool, &state.secrets)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('operator',$1,'Operator',clock_timestamp(),clock_timestamp())")
        .bind(crate::security::token_digest("admin")).execute(&state.pool).await.unwrap();
    let record = json!({"name":"record.example.test","record_type":"A","content":"8.8.8.8","ttl":300,"proxied":false});
    let case = if kind < 4 {
        if kind > 0 {
            sqlx::query("INSERT INTO dns_records(id,zone_id,provider_record_id,name,record_type,content,ttl,proxied,origin,state,remote_snapshot) VALUES('record','zone','remote-record','record.example.test','A','8.8.8.8',300,false,'external','synced',$1)")
                .bind(json!({"id":"remote-record","name":"record.example.test","type":"A","content":"8.8.8.8","ttl":300,"proxied":false})).execute(&state.pool).await.unwrap();
        }
        match kind {
            0 => Case {
                method: Method::POST,
                path: "/dns/records".into(),
                body: json!({"zone_id":"zone","record":record}),
                status: StatusCode::ACCEPTED,
                node: None,
            },
            1 => Case {
                method: Method::PATCH,
                path: "/dns/records/record".into(),
                body: json!({"expected_revision":1,"record":{"content":"8.8.4.4", "name":"record.example.test","record_type":"A","ttl":300,"proxied":false}}),
                status: StatusCode::ACCEPTED,
                node: None,
            },
            2 => Case {
                method: Method::DELETE,
                path: "/dns/records/record".into(),
                body: json!({"expected_revision":1}),
                status: StatusCode::ACCEPTED,
                node: None,
            },
            3 => Case {
                method: Method::POST,
                path: "/dns/records/record/check".into(),
                body: json!({"expected_revision":1}),
                status: StatusCode::ACCEPTED,
                node: None,
            },
            _ => unreachable!(),
        }
    } else {
        let node = crate::dns::tests::node(&state, &ssh).await;
        let mut tx = state.pool.begin().await.unwrap();
        sqlx::query("UPDATE nodes SET deployed_revision=1,deployed_config_enc=desired_config_enc,published_connection=$1 WHERE id=$2")
            .bind(json!({"public_host":"old.example.test","public_port":443,"tls_skip_verify":false})).bind(&node).execute(&mut *tx).await.unwrap();
        crate::credentials::reference_index::flush_node_on(&mut tx, &state.secrets, &node)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let allocation = json!({"zone_id":"zone","mode":"manual","hostname":"bound.example.test","ipv4":"8.8.8.8"});
        let (method, body, status) = if kind == 4 {
            (
                Method::PUT,
                json!({"expected_revision":1,"allocation":allocation}),
                StatusCode::ACCEPTED,
            )
        } else {
            let mut input = json!({"expected_revision":1,"allocation":allocation});
            input["allocation"]["idempotency_key"] = json!("fixture-binding");
            let (_, Json(_)) = crate::api::dns::set_binding(
                State(state.clone()),
                Path(node.clone()),
                Json(serde_json::from_value(input).unwrap()),
            )
            .await
            .unwrap();
            (
                Method::DELETE,
                json!({"expected_revision":2,"public_host":"replacement.example.test"}),
                StatusCode::OK,
            )
        };
        Case {
            method,
            path: format!("/nodes/{node}/dns-binding"),
            body,
            status,
            node: Some(node),
        }
    };
    (state, case)
}
async fn send(client: &Client, base: &str, case: &Case, key: Option<&str>) -> reqwest::Response {
    let mut request = client
        .request(
            case.method.clone(),
            format!("{}{}", base.trim_end_matches("/dns"), case.path),
        )
        .bearer_auth("admin")
        .json(&case.body);
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    request.send().await.unwrap()
}
async fn assert_committed(state: &AppState, case: &Case, kind: usize) {
    assert_eq!(
        count(state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    assert_eq!(count(state, "SELECT count(*) FROM dns_records").await, 1);
    if kind < 4 {
        assert_eq!(count(state, "SELECT count(*) FROM jobs").await, 1);
        assert_eq!(count(state, "SELECT count(*) FROM job_events").await, 1);
        assert_eq!(count(state, "SELECT count(*) FROM dns_operations").await, 1);
        let row = sqlx::query("SELECT revision,state,desired FROM dns_records")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            row.get::<i64, _>("revision"),
            if matches!(kind, 1 | 2) { 2 } else { 1 }
        );
        if kind == 3 {
            assert!(row.get::<Option<Value>, _>("desired").is_none());
        } else {
            assert!(row.get::<Option<Value>, _>("desired").is_some());
        }
    } else {
        let row = sqlx::query(
            "SELECT desired_revision,public_host,published_connection FROM nodes WHERE id=$1",
        )
        .bind(case.node.as_ref().unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            row.get::<Value, _>("published_connection")["public_host"],
            "old.example.test"
        );
        assert_eq!(
            row.get::<i64, _>("desired_revision"),
            if kind == 4 { 2 } else { 3 }
        );
        assert_eq!(
            row.get::<String, _>("public_host"),
            if kind == 4 {
                "bound.example.test"
            } else {
                "replacement.example.test"
            }
        );
        assert_eq!(
            count(state, "SELECT count(*) FROM config_versions").await,
            if kind == 4 { 2 } else { 3 }
        );
        assert_eq!(
            count(state, "SELECT count(*) FROM dns_bindings").await,
            if kind == 4 { 1 } else { 0 }
        );
        assert_eq!(
            count(
                state,
                "SELECT count(*) FROM jobs WHERE kind='sync' AND status='queued'"
            )
            .await,
            1
        );
        assert_eq!(
            count(state, "SELECT count(*) FROM dns_operations").await,
            if kind == 4 { 2 } else { 3 }
        );
        if kind == 5 {
            assert_eq!(count(state,"SELECT count(*) FROM audit_records WHERE action='dns.node.unbound' AND actor='operator'").await,1);
        }
    }
}

#[tokio::test]
async fn six_operations_share_one_committed_result_and_replay_after_all_sources_are_removed() {
    let client = client();
    for kind in 0..6 {
        let (state, case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let (a, b) = tokio::join!(
            send(&client, &base, &case, Some("race")),
            send(&client, &base, &case, Some("race"))
        );
        assert_eq!(a.status(), case.status, "kind {kind}");
        assert_eq!(b.status(), case.status, "kind {kind}");
        let bytes = a.bytes().await.unwrap();
        assert_eq!(b.bytes().await.unwrap(), bytes);
        assert_committed(&state, &case, kind).await;
        sqlx::raw_sql("DELETE FROM dns_operations; DELETE FROM jobs; DELETE FROM nodes; DELETE FROM dns_records; DELETE FROM dns_zones; DELETE FROM dns_connections")
            .execute(&state.pool).await.unwrap();
        let before = snapshot(&state).await;
        let response = send(&client, &base, &case, Some("race")).await;
        assert_eq!(response.status(), case.status);
        assert_eq!(response.bytes().await.unwrap(), bytes);
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn six_mutations_and_their_receipts_roll_back_together_and_native_errors_retry_once() {
    let client = client();
    for code in ["40001", "40P01"] {
        for kind in 0..6 {
            let (state, case) = fixture(kind).await;
            let (base, server) = serve(state.clone()).await;
            sqlx::raw_sql("CREATE FUNCTION reject_record_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER reject_record_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_record_receipt()")
                .execute(&state.pool).await.unwrap();
            let before = snapshot(&state).await;
            assert_eq!(
                send(&client, &base, &case, Some("retry")).await.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "kind {kind}"
            );
            assert_eq!(snapshot(&state).await, before, "kind {kind}");
            sqlx::raw_sql(&format!("CREATE SEQUENCE record_attempt; CREATE OR REPLACE FUNCTION reject_record_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('record_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$"))
                .execute(&state.pool).await.unwrap();
            let response = send(&client, &base, &case, Some("retry")).await;
            assert_eq!(response.status(), case.status, "kind {kind}, code {code}");
            let bytes = response.bytes().await.unwrap();
            assert_committed(&state, &case, kind).await;
            assert_eq!(
                count(&state, "SELECT last_value FROM record_attempt").await,
                2
            );
            let before = snapshot(&state).await;
            assert_eq!(
                send(&client, &base, &case, Some("retry"))
                    .await
                    .bytes()
                    .await
                    .unwrap(),
                bytes
            );
            assert_eq!(snapshot(&state).await, before);
            server.abort();
        }
    }
}

#[tokio::test]
async fn six_missing_keys_or_conflicting_requests_cannot_change_the_source() {
    let client = client();
    for kind in 0..6 {
        let (state, mut case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let before = snapshot(&state).await;
        assert_eq!(
            send(&client, &base, &case, None).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(snapshot(&state).await, before);
        assert_eq!(
            send(&client, &base, &case, Some("same")).await.status(),
            case.status
        );
        let before = snapshot(&state).await;
        case.body["unexpected"] = json!(true);
        assert_eq!(
            send(&client, &base, &case, Some("same")).await.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn bound_record_deletion_and_hostname_changes_remain_blocked_without_receipts() {
    let client = client();
    let (state, binding) = fixture(4).await;
    let (base, server) = serve(state.clone()).await;
    assert_eq!(
        send(&client, &base, &binding, Some("bind")).await.status(),
        StatusCode::ACCEPTED
    );
    sqlx::query("UPDATE dns_records SET desired=NULL,state='synced',provider_record_id='remote-bound',remote_snapshot=jsonb_build_object('name',name,'type',record_type,'content',content,'ttl',ttl,'proxied',proxied)")
        .execute(&state.pool).await.unwrap();
    let record: String = sqlx::query_scalar("SELECT id FROM dns_records")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    for (method, body) in [
        (Method::DELETE, json!({"expected_revision":1})),
        (
            Method::PATCH,
            json!({"expected_revision":1,"record":{"name":"renamed.example.test","record_type":"A","content":"8.8.8.8","ttl":300,"proxied":false}}),
        ),
    ] {
        let case = Case {
            method,
            path: format!("/dns/records/{record}"),
            body,
            status: StatusCode::ACCEPTED,
            node: None,
        };
        assert_eq!(
            send(&client, &base, &case, Some("blocked")).await.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state).await, before);
    }
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    server.abort();
}
