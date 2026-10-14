//! Public pinned binaries only. Atomic writes, checksum verification and bounded disk use.
use anyhow::{Result, bail};
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::Mutex as AsyncMutex;
static LOCKS: OnceLock<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>> = OnceLock::new();
struct Partial(PathBuf);
impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
pub async fn load<F, Fut>(
    name: &str,
    digest: &str,
    verify: fn(&[u8], &str) -> Result<()>,
    download: F,
) -> Result<Vec<u8>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<u8>>>,
{
    if digest.len() != 64
        || !digest.bytes().all(|c| c.is_ascii_hexdigit())
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        bail!("invalid artifact cache key");
    }
    let root = std::env::var_os("HYSTERIAX_ASSET_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("hysteriax-assets"));
    load_in(&root, name, digest, verify, download).await
}

async fn load_in<F, Fut>(
    root: &Path,
    name: &str,
    digest: &str,
    verify: fn(&[u8], &str) -> Result<()>,
    download: F,
) -> Result<Vec<u8>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<u8>>>,
{
    let key = format!("hysteriax-{name}-{digest}.bin");
    let lock = {
        let mut locks = LOCKS.get_or_init(Default::default).lock().unwrap();
        locks.retain(|_, v| v.strong_count() > 0);
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            lock
        } else {
            let lock = Arc::new(AsyncMutex::new(()));
            locks.insert(key.clone(), Arc::downgrade(&lock));
            lock
        }
    };
    let _guard = lock.lock().await;
    let path = root.join(key);
    if tokio::fs::metadata(&path)
        .await
        .is_ok_and(|meta| meta.len() <= 100_000_000)
        && let Ok(bytes) = tokio::fs::read(&path).await
    {
        if verify(&bytes, digest).is_ok() {
            return Ok(bytes);
        }
        let _ = tokio::fs::remove_file(&path).await;
    }
    let bytes = download().await?;
    verify(&bytes, digest)?;
    if tokio::fs::create_dir_all(&root).await.is_err() {
        return Ok(bytes);
    }
    static DISK_WRITE: AsyncMutex<()> = AsyncMutex::const_new(());
    let _write = DISK_WRITE.lock().await;
    trim(root, (300usize * 1024 * 1024).saturating_sub(bytes.len())).await;
    let partial = Partial(root.join(format!("hysteriax-{}.part", uuid::Uuid::new_v4())));
    if tokio::fs::write(&partial.0, &bytes).await.is_ok() {
        let _ = tokio::fs::rename(&partial.0, &path).await;
    }
    Ok(bytes)
}
async fn trim(root: &Path, limit: usize) {
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return;
    };
    let mut files = Vec::new();
    let mut bytes = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("hysteriax-") {
            continue;
        }
        if let Ok(meta) = entry.metadata().await {
            if name.ends_with(".part")
                && meta
                    .modified()
                    .ok()
                    .and_then(|at| at.elapsed().ok())
                    .is_some_and(|age| age.as_secs() > 3600)
            {
                let _ = tokio::fs::remove_file(entry.path()).await;
                continue;
            }
            if name.ends_with(".bin") {
                bytes += meta.len() as usize;
                files.push((meta.modified().ok(), entry.path(), meta.len() as usize));
            }
        }
    }
    files.sort_by_key(|file| file.0);
    for (_, path, size) in files {
        if bytes <= limit {
            break;
        }
        if tokio::fs::remove_file(path).await.is_ok() {
            bytes -= size;
        }
    }
}

#[cfg(test)]
mod tests {
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
}
