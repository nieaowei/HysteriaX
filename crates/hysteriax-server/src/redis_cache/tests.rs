use super::*;
use crate::cache::{Cache, CachedResponse};
use axum::{
    body::Bytes,
    http::{HeaderMap, HeaderValue, StatusCode},
};
use serde_json::json;
fn secrets() -> Arc<SecretBox> {
    Arc::new(SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap())
}
#[tokio::test]
async fn shared_encrypted_cache_survives_instance_replacement_and_expires() {
    let Ok(url) = std::env::var("TEST_REDIS_URL") else {
        return;
    };
    let prefix = format!("test:{}", uuid::Uuid::new_v4());
    let backend = Arc::new(RedisCache::new(&url, prefix, secrets()).unwrap());
    let cache = || {
        Cache::new(4, 4096, 4096, |v: &serde_json::Value| v.to_string().len())
            .with_redis(Some(backend.clone()), "values")
    };
    let first = cache();
    let value = first
        .get(
            "source:1".into(),
            Duration::from_millis(600),
            false,
            || async { Ok::<_, ()>(json!({"secret":"private-marker"})) },
        )
        .await
        .unwrap();
    assert_eq!(value["secret"], "private-marker");
    assert_eq!(first.stats()["entries"], 0);
    let mut connection = redis::Client::open(url.as_str())
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let cipher: String = redis::cmd("GET")
        .arg(backend.key("values", "source:1"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!(!cipher.contains("private-marker"));
    let second = cache();
    assert_eq!(
        second
            .get("source:1".into(), Duration::from_secs(1), false, || async {
                panic!("must reuse Redis");
                #[allow(unreachable_code)]
                Ok::<_, ()>(json!(null))
            })
            .await
            .unwrap()["secret"],
        "private-marker"
    );
    assert_eq!(
        second
            .get("source:2".into(), Duration::from_secs(1), false, || async {
                Ok::<_, ()>(json!("new generation"))
            })
            .await
            .unwrap()
            .as_str(),
        Some("new generation")
    );
    tokio::time::sleep(Duration::from_millis(650)).await;
    assert_eq!(
        second
            .get("source:1".into(), Duration::from_secs(1), false, || async {
                Ok::<_, ()>(json!("expired"))
            })
            .await
            .unwrap()
            .as_str(),
        Some("expired")
    );
    first
        .get("source:1".into(), Duration::from_secs(1), true, || async {
            Ok::<_, ()>(json!("forced"))
        })
        .await
        .unwrap();
    assert_eq!(
        second
            .get("source:1".into(), Duration::from_secs(1), false, || async {
                Ok::<_, ()>(json!("wrong"))
            })
            .await
            .unwrap()
            .as_str(),
        Some("forced")
    );
    let _: () = redis::cmd("SET")
        .arg(backend.key("values", "corrupt"))
        .arg("broken")
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(
        second
            .get("corrupt".into(), Duration::from_secs(1), false, || async {
                Ok::<_, ()>(json!("recovered"))
            })
            .await
            .unwrap()
            .as_str(),
        Some("recovered")
    );
    let mut headers = HeaderMap::new();
    headers.append("x-binary", HeaderValue::from_bytes(&[0x80]).unwrap());
    headers.append("x-multiple", HeaderValue::from_static("a"));
    headers.append("x-multiple", HeaderValue::from_static("b"));
    let response = CachedResponse {
        status: StatusCode::OK,
        headers,
        body: Bytes::from_static(&[0, 128, 255]),
    };
    backend
        .put("test-response", &response, Duration::from_secs(1))
        .await;
    let decoded: CachedResponse = backend.get("test-response").await.unwrap();
    assert_eq!(decoded.body, response.body);
    assert_eq!(decoded.headers, response.headers);
    backend.delete("test-response").await;
}
#[tokio::test]
async fn unavailable_redis_falls_back_without_delaying_every_request() {
    let backend =
        Arc::new(RedisCache::new("redis://127.0.0.1:9", "unavailable".into(), secrets()).unwrap());
    let cache =
        Cache::new(4, 4096, 4096, |v: &String| v.len()).with_redis(Some(backend.clone()), "values");
    let start = Instant::now();
    for _ in 0..3 {
        assert_eq!(
            cache
                .get("same".into(), Duration::from_secs(1), false, || async {
                    Ok::<_, ()>("fresh".to_owned())
                })
                .await
                .unwrap()
                .as_str(),
            "fresh"
        );
    }
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(!backend.available());
    assert_eq!(cache.stats()["hits"], 2);
}
