use super::*;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
fn verify(bytes: &[u8], digest: &str) -> Result<()> {
    if hex::encode(Sha256::digest(bytes)) != digest {
        bail!("digest mismatch");
    }
    Ok(())
}
#[tokio::test]
async fn concurrent_downloads_corrupt_cache_and_failures_are_safe() {
    let root =
        std::env::temp_dir().join(format!("hysteriax-artifact-test-{}", uuid::Uuid::new_v4()));
    let bytes = vec![7; 1000];
    let digest = hex::encode(Sha256::digest(&bytes));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let root = root.clone();
        let bytes = bytes.clone();
        let digest = digest.clone();
        let calls = calls.clone();
        tasks.spawn(async move {
            load_in(&root, "asset", &digest, verify, || async {
                calls.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                Ok(bytes)
            })
            .await
            .unwrap()
        });
    }
    while let Some(value) = tasks.join_next().await {
        assert_eq!(value.unwrap(), bytes);
    }
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let path = root.join(format!("hysteriax-asset-{digest}.bin"));
    tokio::fs::write(&path, b"corrupt").await.unwrap();
    assert!(
        load_in(&root, "asset", &digest, verify, || async {
            Err(anyhow::anyhow!("offline"))
        })
        .await
        .is_err()
    );
    assert!(!path.exists());
    assert_eq!(
        load_in(&root, "asset", &digest, verify, || async {
            Ok(bytes.clone())
        })
        .await
        .unwrap(),
        bytes
    );
    tokio::fs::remove_dir_all(root).await.unwrap();
}
