use super::*;
#[tokio::test]
async fn single_flight_bypass_and_source_versions() {
    let cache = Arc::new(Cache::new(4, 128, 128, |v: &String| v.len()));
    let calls = Arc::new(AtomicU64::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let cache = cache.clone();
        let calls = calls.clone();
        tasks.spawn(async move {
            cache
                .get(
                    "source:1".into(),
                    Duration::from_secs(10),
                    false,
                    || async {
                        calls.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Ok::<_, ()>("one".to_owned())
                    },
                )
                .await
                .unwrap()
        });
    }
    while let Some(value) = tasks.join_next().await {
        assert_eq!(value.unwrap().as_str(), "one");
    }
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let fresh = cache
        .get("source:1".into(), Duration::from_secs(10), true, || async {
            Ok::<_, ()>("refresh".into())
        })
        .await
        .unwrap();
    assert_eq!(fresh.as_str(), "refresh");
    let changed = cache
        .get(
            "source:2".into(),
            Duration::from_secs(10),
            false,
            || async { Ok::<_, ()>("changed".into()) },
        )
        .await
        .unwrap();
    assert_eq!(changed.as_str(), "changed");
}
#[tokio::test]
async fn cancelled_failed_expired_and_oversized_entries_do_not_poison_cache() {
    let cache = Arc::new(Cache::new(1, 8, 8, |v: &String| v.len()));
    let entered = Arc::new(tokio::sync::Notify::new());
    let task = {
        let cache = cache.clone();
        let entered = entered.clone();
        tokio::spawn(async move {
            cache
                .get("cancel".into(), Duration::from_secs(5), false, || async {
                    entered.notify_one();
                    std::future::pending::<Result<String, ()>>().await
                })
                .await
        })
    };
    entered.notified().await;
    task.abort();
    let _ = task.await;
    assert_eq!(
        cache
            .get("next".into(), Duration::ZERO, false, || async {
                Ok::<_, ()>("next".into())
            })
            .await
            .unwrap()
            .as_str(),
        "next"
    );
    assert!(
        cache
            .get("bad".into(), Duration::from_secs(1), false, || async {
                Err::<String, _>(())
            })
            .await
            .is_err()
    );
    assert_eq!(
        cache
            .get("bad".into(), Duration::from_secs(1), false, || async {
                Ok::<_, ()>("fixed".into())
            })
            .await
            .unwrap()
            .as_str(),
        "fixed"
    );
    cache
        .get("large".into(), Duration::from_secs(1), false, || async {
            Ok::<_, ()>("too-large-for-cache".into())
        })
        .await
        .unwrap();
    assert_eq!(cache.stats()["entries"], 0);
}
