//! Bounded, cancellation-safe single-flight caches. Keys include committed source versions.
use crate::{api::AdminActor, error::ApiError, state::AppState};
use axum::{
    body::{Body, Bytes, HttpBody, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::OnceCell;

struct Stored<T> {
    value: Arc<T>,
    expires: Instant,
}
struct Slot<T> {
    cell: OnceCell<Stored<T>>,
    created: Instant,
    active: AtomicU64,
}
struct Entry<T> {
    slot: Arc<Slot<T>>,
    bytes: usize,
}
struct Inner<T> {
    entries: HashMap<String, Entry<T>>,
    bytes: usize,
}
#[derive(Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    waits: AtomicU64,
    evictions: AtomicU64,
}
pub struct Cache<T> {
    redis: Option<(Arc<crate::redis_cache::RedisCache>, String)>,
    inner: Mutex<Inner<T>>,
    counters: Counters,
    max_entries: usize,
    max_bytes: usize,
    max_value_bytes: usize,
    weight: fn(&T) -> usize,
}
impl<T: serde::Serialize + serde::de::DeserializeOwned> Cache<T> {
    pub fn new(
        max_entries: usize,
        max_bytes: usize,
        max_value_bytes: usize,
        weight: fn(&T) -> usize,
    ) -> Self {
        Self {
            redis: None,
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
                bytes: 0,
            }),
            counters: Counters::default(),
            max_entries,
            max_bytes,
            max_value_bytes,
            weight,
        }
    }
    pub fn with_redis(
        mut self,
        backend: Option<Arc<crate::redis_cache::RedisCache>>,
        kind: &str,
    ) -> Self {
        self.redis = backend.map(|r| (r, kind.to_owned()));
        self
    }
    fn remove(&self, inner: &mut Inner<T>, key: &str) {
        if let Some(entry) = inner.entries.remove(key) {
            inner.bytes -= entry.bytes;
            self.counters.evictions.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn evict(&self, inner: &mut Inner<T>, except: &str) -> bool {
        let key = inner
            .entries
            .iter()
            .filter(|(key, e)| {
                key.as_str() != except
                    && (e.slot.cell.get().is_some() || e.slot.active.load(Ordering::Relaxed) == 0)
            })
            .min_by_key(|(_, e)| e.slot.created)
            .map(|(key, _)| key.clone());
        if let Some(key) = key {
            self.remove(inner, &key);
            true
        } else {
            false
        }
    }
    pub async fn get<E, F, Fut>(
        &self,
        key: String,
        ttl: Duration,
        bypass: bool,
        compute: F,
    ) -> Result<Arc<T>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        self.get_until(key, ttl, None, bypass, compute).await
    }

    /// Absolute validity boundary, including time spent computing the value.
    pub async fn get_until<E, F, Fut>(
        &self,
        key: String,
        ttl: Duration,
        deadline: Option<Instant>,
        bypass: bool,
        compute: F,
    ) -> Result<Arc<T>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        if !crate::read_snapshot::cacheable() || deadline.is_some_and(|at| at <= Instant::now()) {
            return compute().await.map(Arc::new);
        }
        let remote_key = self.redis.as_ref().map(|(r, kind)| r.key(kind, &key));
        if let Some((remote, _)) = &self.redis {
            let remote_key = remote_key.as_ref().unwrap();
            if bypass {
                remote.delete(remote_key).await;
            } else if let Some(value) = remote.get(remote_key).await {
                self.counters.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(Arc::new(value));
            }
        }
        let slot = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(entry) = inner.entries.get(&key)
                && let Some(stored) = entry.slot.cell.get()
            {
                if !bypass && stored.expires > Instant::now() {
                    self.counters.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(stored.value.clone());
                }
                self.remove(&mut inner, &key);
            }
            if let Some(entry) = inner.entries.get(&key) {
                self.counters.waits.fetch_add(1, Ordering::Relaxed);
                entry.slot.active.fetch_add(1, Ordering::Relaxed);
                Some(entry.slot.clone())
            } else {
                while inner.entries.len() >= self.max_entries {
                    if !self.evict(&mut inner, &key) {
                        break;
                    }
                }
                if inner.entries.len() >= self.max_entries {
                    None
                } else {
                    self.counters.misses.fetch_add(1, Ordering::Relaxed);
                    let slot = Arc::new(Slot {
                        cell: OnceCell::new(),
                        created: Instant::now(),
                        active: AtomicU64::new(1),
                    });
                    inner.entries.insert(
                        key.clone(),
                        Entry {
                            slot: slot.clone(),
                            bytes: 0,
                        },
                    );
                    Some(slot)
                }
            }
        };
        let Some(slot) = slot else {
            return compute().await.map(Arc::new);
        };
        struct ActiveGuard<'a>(&'a AtomicU64);
        impl Drop for ActiveGuard<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let _active = ActiveGuard(&slot.active);
        let stored = match slot
            .cell
            .get_or_try_init(|| async {
                let value = compute().await?;
                let mut lifetime = deadline
                    .map(|at| at.saturating_duration_since(Instant::now()))
                    .unwrap_or(ttl)
                    .min(ttl);
                if let Some((remote, _)) = &self.redis {
                    if (self.weight)(&value) <= self.max_value_bytes && !lifetime.is_zero() {
                        remote
                            .put(remote_key.as_ref().unwrap(), &value, lifetime)
                            .await;
                    }
                    lifetime = if remote.available() {
                        Duration::ZERO
                    } else {
                        lifetime.min(Duration::from_secs(2))
                    };
                }
                Ok(Stored {
                    value: Arc::new(value),
                    expires: deadline
                        .map(|at| at.min(Instant::now() + lifetime))
                        .unwrap_or_else(|| Instant::now() + lifetime),
                })
            })
            .await
        {
            Ok(value) => value,
            Err(error) => {
                let mut inner = self.inner.lock().unwrap();
                if inner
                    .entries
                    .get(&key)
                    .is_some_and(|e| Arc::ptr_eq(&e.slot, &slot))
                {
                    self.remove(&mut inner, &key);
                }
                return Err(error);
            }
        };
        let mut inner = self.inner.lock().unwrap();
        if inner
            .entries
            .get(&key)
            .is_some_and(|e| Arc::ptr_eq(&e.slot, &slot) && e.bytes == 0)
        {
            let bytes = (self.weight)(&stored.value).max(1);
            if bytes > self.max_value_bytes || bytes > self.max_bytes {
                self.remove(&mut inner, &key);
            } else {
                while inner.bytes + bytes > self.max_bytes {
                    if !self.evict(&mut inner, &key) {
                        break;
                    }
                }
                inner.entries.get_mut(&key).unwrap().bytes = bytes;
                inner.bytes += bytes;
            }
        }
        if self.redis.as_ref().is_some_and(|(r, _)| r.available())
            && let Some(entry) = inner.entries.remove(&key)
        {
            inner.bytes -= entry.bytes;
        }
        Ok(stored.value.clone())
    }
    pub fn stats(&self) -> serde_json::Value {
        let inner = self.inner.lock().unwrap();
        serde_json::json!({"entries":inner.entries.len(),"bytes":inner.bytes,
            "hits":self.counters.hits.load(Ordering::Relaxed),"misses":self.counters.misses.load(Ordering::Relaxed),
            "waits":self.counters.waits.load(Ordering::Relaxed),"evictions":self.counters.evictions.load(Ordering::Relaxed)})
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "WireResponse", into = "WireResponse")]
pub struct CachedResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}
impl CachedResponse {
    pub async fn capture(response: Response) -> Result<Self, Box<Response>> {
        if response.status() != StatusCode::OK
            || response
                .body()
                .size_hint()
                .upper()
                .is_none_or(|size| size > 2 * 1024 * 1024)
        {
            return Err(Box::new(response));
        }
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, 2 * 1024 * 1024)
            .await
            .map_err(|_| Box::new(ApiError::internal().into_response()))?;
        Ok(Self {
            status: parts.status,
            headers: parts.headers,
            body: bytes,
        })
    }
    pub fn response(&self) -> Response {
        let mut response = Response::new(Body::from(self.body.clone()));
        *response.status_mut() = self.status;
        *response.headers_mut() = self.headers.clone();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
        response
    }
}

#[derive(Default, serde::Serialize)]
pub struct RouteStats {
    requests: u64,
    computed: u64,
    reused: u64,
    errors: u64,
    total_us: u64,
    max_us: u64,
}
pub struct RuntimeCaches {
    routes: Mutex<std::collections::BTreeMap<String, RouteStats>>,
    pub redis: Option<Arc<crate::redis_cache::RedisCache>>,
    pub responses: Cache<CachedResponse>,
    pub values: Cache<serde_json::Value>,
    pub previews: Cache<serde_json::Value>,
}
impl RuntimeCaches {
    pub fn new(secrets: Arc<crate::security::SecretBox>) -> Self {
        let redis = std::env::var("HYSTERIAX_REDIS_URL").ok().map(|url| {
            let namespace =
                crate::security::token_digest(&std::env::var("DATABASE_URL").unwrap_or_default());
            Arc::new(
                crate::redis_cache::RedisCache::new(
                    &url,
                    format!("hysteriax:cache:3:{namespace}"),
                    secrets,
                )
                .unwrap_or_else(|_| panic!("invalid HYSTERIAX_REDIS_URL")),
            )
        });
        Self {
            routes: Mutex::new(std::collections::BTreeMap::new()),
            redis: redis.clone(),
            responses: Cache::new(
                256,
                32 * 1024 * 1024,
                2 * 1024 * 1024,
                |v: &CachedResponse| v.body.len(),
            )
            .with_redis(redis.clone(), "responses"),
            values: Cache::new(32, 16 * 1024 * 1024, 4 * 1024 * 1024, |v| {
                serde_json::to_vec(v).map(|v| v.len()).unwrap_or(0)
            })
            .with_redis(redis.clone(), "values"),
            previews: Cache::new(256, 16 * 1024 * 1024, 1024 * 1024, |v| {
                serde_json::to_vec(v).map(|v| v.len()).unwrap_or(0)
            })
            .with_redis(redis, "previews"),
        }
    }
    fn record(&self, route: String, computed: bool, elapsed: Duration, error: bool) {
        let mut routes = self.routes.lock().unwrap();
        let stat = routes.entry(route).or_default();
        stat.requests += 1;
        stat.computed += u64::from(computed);
        stat.reused += u64::from(!computed);
        stat.errors += u64::from(error);
        let micros = elapsed.as_micros() as u64;
        stat.total_us += micros;
        stat.max_us = stat.max_us.max(micros);
    }
    pub fn stats(&self) -> serde_json::Value {
        serde_json::json!({"routes":*self.routes.lock().unwrap(),"backend":if self.redis.is_some() {"redis"} else {"memory"},"redis":self.redis.as_ref().map(|r|r.stats()),"responses":self.responses.stats(),"values":self.values.stats(),"previews":self.previews.stats()})
    }
}
#[cfg(test)]
pub async fn generation(pool: &sqlx::PgPool) -> Result<i64, ApiError> {
    Ok(
        sqlx::query_scalar("SELECT coalesce(sum(generation),0)::bigint FROM cache_bucket_versions")
            .fetch_one(pool)
            .await?,
    )
}
fn ttl(path: &str) -> Option<Duration> {
    if path.starts_with("/api/v1/subscription-templates")
        || path.ends_with("/events")
        || path == "/api/v1/events"
        || path == "/api/v1/version"
        || path.starts_with("/api/v1/admin/")
        || path.ends_with("/subscription")
        || path == "/api/v1/server/monitoring"
        || path == "/api/v1/cache"
    {
        return None;
    }
    Some(Duration::from_secs(if path == "/api/v1/overview/history" {
        60
    } else if path == "/api/v1/overview" || path == "/api/v1/overview/nodes" {
        3
    } else if path.starts_with("/api/v1/jobs") || path.starts_with("/api/v1/credential-batches") {
        1
    } else {
        5
    }))
}
pub async fn middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if request.method() != Method::GET || ttl(request.uri().path()).is_none() {
        return next.run(request).await;
    }
    let path = request.uri().path();
    let prepared = if path.starts_with("/api/v1/credentials") {
        crate::credentials::reference_index::begin_read_transaction(&state.pool, &state.secrets)
            .await
    } else if path == "/api/v1/overview/history" {
        let query: std::collections::BTreeMap<_, _> =
            url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes()).collect();
        let source = query.get("source").map(|v| v.as_ref()).unwrap_or("users");
        let metrics = query.get("metrics").map(|v| v.as_ref()).unwrap_or("all");
        crate::analytics::snapshot_transaction(
            &state.pool,
            chrono::Utc::now(),
            metrics != "traffic" || source == "users",
        )
        .await
    } else {
        crate::read_snapshot::transaction(&state.pool)
            .await
            .map_err(Into::into)
    };
    let tx = match prepared {
        Ok(tx) => tx,
        Err(error) => return error.into_response(),
    };
    match crate::read_snapshot::scope_with(tx, async {
        Ok(cached_middleware(state, request, next).await)
    })
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
async fn cached_middleware(state: AppState, request: Request, next: Next) -> Response {
    if request.method() != Method::GET {
        return next.run(request).await;
    }
    let Some(ttl) = ttl(request.uri().path()) else {
        return next.run(request).await;
    };
    let Some(actor) = request.extensions().get::<AdminActor>() else {
        return ApiError::internal().into_response();
    };
    let controls = request
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if controls.split(',').any(|v| v.trim() == "no-store") {
        return BYPASS.scope(true, next.run(request)).await;
    }
    let started = Instant::now();
    let computed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let route = crate::cache_domains::route(request.uri().path());
    let bypass = controls.split(',').any(|v| v.trim() == "no-cache");
    let mut query: Vec<_> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    query.sort();
    let (version, ttl) =
        match crate::cache_domains::policy(&state, request.uri().path(), &query, ttl).await {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };

    let key = format!(
        "{}|{}|{}|{:?}",
        actor.id,
        version,
        request.uri().path(),
        query
    );
    let response = match state
        .caches
        .responses
        .get(key, ttl, bypass, || async {
            computed.store(true, Ordering::Relaxed);
            CachedResponse::capture(BYPASS.scope(bypass, next.run(request)).await).await
        })
        .await
    {
        Ok(cached) => cached.response(),
        Err(response) => *response,
    };
    state.caches.record(
        route,
        computed.load(Ordering::Relaxed),
        started.elapsed(),
        response.status().is_server_error(),
    );
    response
}

#[cfg(test)]
mod tests {
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
}

pub async fn configuration_generation(pool: &sqlx::PgPool) -> Result<i64, ApiError> {
    Ok(
        sqlx::query_scalar("SELECT coalesce(sum(generation),0)::bigint FROM cache_bucket_versions WHERE domain='directory'")
            .fetch_one(pool)
            .await?,
    )
}

tokio::task_local! { static BYPASS: bool; }
pub fn bypass() -> bool {
    BYPASS.try_with(|value| *value).unwrap_or(false)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct WireResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}
impl From<CachedResponse> for WireResponse {
    fn from(value: CachedResponse) -> Self {
        use base64::Engine;
        Self {
            status: value.status.as_u16(),
            headers: value
                .headers
                .iter()
                .map(|(name, value)| {
                    (
                        name.to_string(),
                        base64::engine::general_purpose::STANDARD.encode(value.as_bytes()),
                    )
                })
                .collect(),
            body: base64::engine::general_purpose::STANDARD.encode(value.body),
        }
    }
}
impl TryFrom<WireResponse> for CachedResponse {
    type Error = &'static str;
    fn try_from(value: WireResponse) -> Result<Self, Self::Error> {
        use base64::Engine;
        let decode = |text: &str| {
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|_| "invalid cache encoding")
        };
        let mut headers = HeaderMap::new();
        for (name, value) in value.headers {
            headers.append(
                axum::http::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| "invalid cache header")?,
                axum::http::HeaderValue::from_bytes(&decode(&value)?)
                    .map_err(|_| "invalid cache header")?,
            );
        }
        let body = decode(&value.body)?;
        if body.len() > 2 * 1024 * 1024 {
            return Err("oversized cache response");
        }
        Ok(Self {
            status: StatusCode::from_u16(value.status).map_err(|_| "invalid cache status")?,
            headers,
            body: Bytes::from(body),
        })
    }
}
