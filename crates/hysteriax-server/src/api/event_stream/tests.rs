use super::*;
use reqwest::Client;

async fn fixture() -> AppState {
    let pool = db::test_pool_with_max_connections(8).await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('stream-operator',$1,'Stream operator',clock_timestamp(),clock_timestamp())")
        .bind(token_digest("stream-admin-v2")).execute(&pool).await.unwrap();
    sqlx::raw_sql("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('a','sync','queued','queued',now(),now(),now()),('b','sync','queued','queued',now(),now(),now())")
        .execute(&pool).await.unwrap();
    AppState::new(
        pool,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    )
}
async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v2", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, crate::api::router(state))
            .await
            .unwrap()
    });
    (base, server)
}
fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}
async fn cursor(state: &AppState, sequence: i64) -> Cursor {
    let epoch = sqlx::query_scalar("SELECT epoch FROM event_publication_stream WHERE id")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    Cursor { epoch, sequence }
}
async fn publish(state: &AppState) {
    while crate::events::publish_pending(&state.pool).await.unwrap() == 256 {}
}
async fn append(state: &AppState, job: &str, kind: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES($1,$2,'{}',clock_timestamp()) RETURNING id")
        .bind(job).bind(kind).fetch_one(&state.pool).await.unwrap()
}
#[derive(Debug)]
struct Frame {
    id: Option<String>,
    kind: String,
    data: Value,
}
async fn frames(response: &mut reqwest::Response, count: usize) -> Vec<Frame> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        let mut frames = Vec::new();
        while frames.len() < count {
            bytes.extend(
                response
                    .chunk()
                    .await
                    .unwrap()
                    .expect("stream ended too soon"),
            );
            while let Some(end) = bytes.windows(2).position(|v| v == b"\n\n") {
                let block = String::from_utf8(bytes.drain(..end + 2).collect()).unwrap();
                let mut id = None;
                let mut kind = None;
                let mut data = None;
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("id: ") {
                        id = Some(v.to_owned());
                    }
                    if let Some(v) = line.strip_prefix("event: ") {
                        kind = Some(v.to_owned());
                    }
                    if let Some(v) = line.strip_prefix("data: ") {
                        data = Some(serde_json::from_str(v).unwrap());
                    }
                }
                if let (Some(kind), Some(data)) = (kind, data) {
                    frames.push(Frame { id, kind, data });
                }
            }
        }
        frames
    })
    .await
    .expect("event delivery timed out")
}

#[test]
fn cursor_strings_are_canonical_and_ambiguous_resume_inputs_are_rejected() {
    let epoch = "77777777-7777-4777-8777-777777777777";
    let valid = format!("v2:{epoch}:42");
    assert_eq!(Cursor::parse(&valid).unwrap().wire(), valid);
    for bad in [
        "42".to_owned(),
        format!("v1:{epoch}:42"),
        format!("v2:{epoch}:-1"),
        format!("v2:{epoch}:01"),
        format!("v2:{epoch}:+1"),
        format!("v2:{epoch}:9223372036854775808"),
        format!("v2:{epoch}:1:2"),
        "v2:invalid:1".into(),
    ] {
        assert_eq!(
            Cursor::parse(&bad).unwrap_err().status,
            StatusCode::BAD_REQUEST
        );
    }
    let mut headers = HeaderMap::new();
    headers.append("last-event-id", valid.parse().unwrap());
    headers.append("last-event-id", valid.parse().unwrap());
    assert_eq!(
        request_cursor(&StreamQuery::default(), &headers)
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn late_small_source_ids_resume_on_another_instance_and_repeated_delivery_keeps_stable_ids() {
    let state = fixture().await;
    let (a, sa) = serve(state.clone()).await;
    let (b, sb) = serve(AppState::new(state.pool.clone(), (*state.secrets).clone())).await;
    let (c, sc) = serve(AppState::new(state.pool.clone(), (*state.secrets).clone())).await;
    let client = client();
    let head = client
        .get(format!("{a}/events/head"))
        .bearer_auth("stream-admin-v2")
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.headers()["cache-control"], "no-store");
    let head: Value = head.json().await.unwrap();
    assert_eq!(head["cursor"], cursor(&state, 0).await.wire());
    let mut late = state.pool.begin().await.unwrap();
    let first:i64=sqlx::query_scalar("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('a','late','{}',clock_timestamp()) RETURNING id").fetch_one(&mut *late).await.unwrap();
    let second = append(&state, "b", "early").await;
    assert!(first < second);
    publish(&state).await;
    let mut response = client
        .get(format!("{a}/events"))
        .query(&[("after", head["cursor"].as_str().unwrap())])
        .bearer_auth("stream-admin-v2")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let delivered = frames(&mut response, 1).await;
    assert_eq!(delivered[0].kind, "early");
    assert_eq!(delivered[0].data["event_id"], second);
    let resume = delivered[0].id.clone().unwrap();
    assert_eq!(resume, cursor(&state, 1).await.wire());
    drop(response);
    late.commit().await.unwrap();
    publish(&state).await;
    let fresh_head: Value = client
        .get(format!("{a}/events/head"))
        .bearer_auth("stream-admin-v2")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fresh_head["cursor"], cursor(&state, 2).await.wire());
    let mut response = client
        .get(format!("{b}/events"))
        .header("last-event-id", &resume)
        .bearer_auth("stream-admin-v2")
        .send()
        .await
        .unwrap();
    let delivered = frames(&mut response, 1).await;
    assert_eq!(delivered[0].kind, "late");
    assert_eq!(delivered[0].data["event_id"], first);
    assert_eq!(
        delivered[0].id.as_deref(),
        Some(cursor(&state, 2).await.wire().as_str())
    );
    drop(response);
    let mut response = client
        .get(format!("{c}/events"))
        .query(&[("after", head["cursor"].as_str().unwrap())])
        .bearer_auth("stream-admin-v2")
        .send()
        .await
        .unwrap();
    let repeated = frames(&mut response, 2).await;
    assert_eq!(
        repeated
            .iter()
            .map(|e| e.data["event_id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![second, first]
    );
    drop(response);
    sa.abort();
    sb.abort();
    sc.abort();
}

#[tokio::test]
async fn global_pages_are_bounded_and_job_filters_publish_checkpoints_without_hiding_gaps() {
    let state = fixture().await;
    sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) SELECT CASE WHEN n%2=1 THEN 'a' ELSE 'b' END,'page',jsonb_build_object('n',n),clock_timestamp() FROM generate_series(1,600) n")
        .execute(&state.pool).await.unwrap();
    publish(&state).await;
    let start = cursor(&state, 0).await;
    let first = batch(&state, &start, "stream-operator", Some("a"))
        .await
        .unwrap();
    assert_eq!(first.through.sequence, 256);
    assert_eq!(first.events.len(), 128);
    assert!(first.more);
    let second = batch(&state, &first.through, "stream-operator", Some("a"))
        .await
        .unwrap();
    assert_eq!(second.through.sequence, 512);
    assert_eq!(second.events.len(), 128);
    assert!(second.more);
    let third = batch(&state, &second.through, "stream-operator", Some("a"))
        .await
        .unwrap();
    assert_eq!(third.through.sequence, 600);
    assert_eq!(third.events.len(), 44);
    assert!(!third.more);
    let (base, server) = serve(state.clone()).await;
    let mut response = client()
        .get(format!("{base}/jobs/a/events"))
        .query(&[("after", start.wire())])
        .bearer_auth("stream-admin-v2")
        .send()
        .await
        .unwrap();
    let delivered = frames(&mut response, 129).await;
    assert!(
        delivered
            .iter()
            .filter(|e| e.kind == "page")
            .all(|e| e.data["n"].as_i64().unwrap() % 2 == 1)
    );
    assert_eq!(delivered[128].kind, "stream.cursor");
    assert_eq!(
        delivered[128].id.as_deref(),
        Some(cursor(&state, 256).await.wire().as_str())
    );
    drop(response);
    // A missing publication for the other job must also invalidate job a's cursor.
    sqlx::query("DELETE FROM event_publications WHERE publication_seq=2")
        .execute(&state.pool)
        .await
        .unwrap();
    let error = batch(&state, &start, "stream-operator", Some("a"))
        .await
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::GONE);
    server.abort();
}

#[tokio::test]
async fn stable_event_ids_cannot_be_replaced_by_a_payload_field() {
    let state = fixture().await;
    let ids: Vec<i64> = sqlx::query_scalar("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('a','plain','{}',clock_timestamp()),('a','object','{\"event_id\":999}',clock_timestamp()) RETURNING id")
        .fetch_all(&state.pool).await.unwrap();
    publish(&state).await;
    let page = batch(&state, &cursor(&state, 0).await, "stream-operator", None)
        .await
        .unwrap();
    let plain: Value = serde_json::from_str(&page.events[0].wire_data()).unwrap();
    let object: Value = serde_json::from_str(&page.events[1].wire_data()).unwrap();
    assert_eq!(plain, json!({"event_id":ids[0]}));
    assert_eq!(object["event_id"], ids[1]);
}

#[tokio::test]
async fn invalid_or_unavailable_cursors_return_http_errors_before_opening_the_stream() {
    let state = fixture().await;
    for _ in 0..3 {
        append(&state, "a", "event").await;
    }
    publish(&state).await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let url = format!("{base}/events");
    for (after, status) in [
        (None, StatusCode::BAD_REQUEST),
        (Some("1".to_owned()), StatusCode::BAD_REQUEST),
        (Some(format!("v2:{}:0", Uuid::new_v4())), StatusCode::GONE),
        (Some(cursor(&state, 4).await.wire()), StatusCode::GONE),
    ] {
        let mut request = client.get(&url).bearer_auth("stream-admin-v2");
        if let Some(after) = after {
            request = request.query(&[("after", after)]);
        }
        assert_eq!(request.send().await.unwrap().status(), status);
    }
    let response = client
        .get(&url)
        .query(&[("after", cursor(&state, 0).await.wire())])
        .header("last-event-id", cursor(&state, 1).await.wire())
        .bearer_auth("stream-admin-v2")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    sqlx::query("DELETE FROM event_publications WHERE publication_seq=2")
        .execute(&state.pool)
        .await
        .unwrap();
    let response = client
        .get(&url)
        .query(&[("after", cursor(&state, 0).await.wire())])
        .bearer_auth("stream-admin-v2")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::GONE);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "event_cursor_expired"
    );
    // A missing final publication cannot masquerade as an empty live stream.
    sqlx::query("DELETE FROM event_publications WHERE publication_seq=3")
        .execute(&state.pool)
        .await
        .unwrap();
    let error = batch(&state, &cursor(&state, 2).await, "stream-operator", None)
        .await
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::GONE);
    server.abort();
}

#[tokio::test]
async fn head_and_resume_reads_do_not_take_the_publishers_lock_or_publish_uncommitted_events() {
    let state = fixture().await;
    let start = cursor(&state, 0).await;
    let mut writer = state.pool.begin().await.unwrap();
    sqlx::query("SELECT head FROM event_publication_stream WHERE id FOR UPDATE")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('a','uncommitted','{}',clock_timestamp())").execute(&mut *writer).await.unwrap();
    let (base, server) = serve(state.clone()).await;
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        client()
            .get(format!("{base}/events/head"))
            .bearer_auth("stream-admin-v2")
            .send(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        response.json::<Value>().await.unwrap()["cursor"],
        start.wire()
    );
    let page = tokio::time::timeout(
        Duration::from_secs(2),
        batch(&state, &start, "stream-operator", None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(page.through, start);
    assert!(page.events.is_empty());
    writer.rollback().await.unwrap();
    assert_eq!(
        crate::events::publish_pending(&state.pool).await.unwrap(),
        0
    );
    server.abort();
}

#[tokio::test]
async fn an_epoch_change_resets_a_live_stream_without_acknowledging_a_new_cursor() {
    let state = fixture().await;
    append(&state, "a", "first").await;
    publish(&state).await;
    let (base, server) = serve(state.clone()).await;
    let mut response = client()
        .get(format!("{base}/events"))
        .query(&[("after", cursor(&state, 0).await.wire())])
        .bearer_auth("stream-admin-v2")
        .send()
        .await
        .unwrap();
    assert_eq!(frames(&mut response, 1).await[0].kind, "first");
    sqlx::query("UPDATE event_publication_stream SET epoch=$1 WHERE id")
        .bind(Uuid::new_v4().to_string())
        .execute(&state.pool)
        .await
        .unwrap();
    let reset = frames(&mut response, 1).await;
    assert_eq!(reset[0].kind, "stream.reset");
    assert!(reset[0].id.is_none());
    assert_eq!(reset[0].data["code"], "event_cursor_expired");
    assert!(response.chunk().await.unwrap().is_none());
    server.abort();
}

#[tokio::test]
async fn a_revoked_administrator_cannot_continue_a_live_stream_even_without_a_local_notice() {
    let state = fixture().await;
    append(&state, "a", "first").await;
    publish(&state).await;
    let (base, server) = serve(state.clone()).await;
    let mut response = client()
        .get(format!("{base}/events"))
        .query(&[("after", cursor(&state, 0).await.wire())])
        .bearer_auth("stream-admin-v2")
        .send()
        .await
        .unwrap();
    frames(&mut response, 1).await;
    sqlx::query("UPDATE admin_tokens SET revoked_at=clock_timestamp() WHERE id='stream-operator'")
        .execute(&state.pool)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(chunk) = response.chunk().await.unwrap() {
            assert!(chunk.is_empty());
        }
    })
    .await
    .unwrap();
    server.abort();
}
