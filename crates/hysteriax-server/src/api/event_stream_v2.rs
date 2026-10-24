use super::*;
use crate::events::PersistedEvent;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Cursor {
    epoch: String,
    sequence: i64,
}
impl Cursor {
    fn parse(value: &str) -> Result<Self, ApiError> {
        let mut parts = value.split(':');
        if parts.next() != Some("v2") {
            return Err(ApiError::bad_request("a v2 event cursor is required"));
        }
        let epoch = parts.next().unwrap_or_default();
        let sequence = parts.next().unwrap_or_default();
        let uuid = Uuid::parse_str(epoch)
            .map_err(|_| ApiError::bad_request("invalid event cursor epoch"))?;
        let number = sequence
            .parse::<i64>()
            .map_err(|_| ApiError::bad_request("invalid event publication sequence"))?;
        if parts.next().is_some()
            || uuid.to_string() != epoch
            || number < 0
            || number.to_string() != sequence
        {
            return Err(ApiError::bad_request("invalid event cursor"));
        }
        Ok(Self {
            epoch: epoch.to_owned(),
            sequence: number,
        })
    }
    fn wire(&self) -> String {
        format!("v2:{}:{}", self.epoch, self.sequence)
    }
}

#[derive(Deserialize, Default)]
pub(super) struct StreamQuery {
    after: Option<String>,
    job_id: Option<String>,
}
fn request_cursor(query: &StreamQuery, headers: &HeaderMap) -> Result<Cursor, ApiError> {
    let mut values = headers.get_all("last-event-id").iter();
    let header = values
        .next()
        .map(|v| v.to_str())
        .transpose()
        .map_err(|_| ApiError::bad_request("invalid Last-Event-ID"))?;
    if values.next().is_some()
        || (query.after.is_some() && header.is_some_and(|h| Some(h) != query.after.as_deref()))
    {
        return Err(ApiError::bad_request("conflicting event cursors"));
    }
    Cursor::parse(
        query.after.as_deref().or(header).ok_or_else(|| {
            ApiError::bad_request("load the event stream head before subscribing")
        })?,
    )
}
fn expired() -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        "event_cursor_expired",
        "event cursor is no longer available; reload the stream head and complete state before subscribing",
    )
}

pub(super) async fn head(State(state): State<AppState>) -> Result<Response, ApiError> {
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let (epoch, sequence): (String, i64) =
        sqlx::query_as("SELECT epoch,head FROM event_publication_stream WHERE id")
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    let cursor = Cursor { epoch, sequence };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(
            json!({"cursor":cursor.wire(),"epoch":cursor.epoch,"publication_seq":cursor.sequence}),
        ),
    )
        .into_response())
}

struct Batch {
    through: Cursor,
    events: Vec<PersistedEvent>,
    more: bool,
}

// Read the next global publication page before applying a job filter. This
// checks continuity in O(page size), including deleted rows and missing tails,
// without scanning all history or acquiring the publisher's stream lock.
async fn batch(
    state: &AppState,
    cursor: &Cursor,
    actor: &str,
    job: Option<&str>,
) -> Result<Batch, ApiError> {
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM admin_tokens WHERE id=$1 AND revoked_at IS NULL)",
    )
    .bind(actor)
    .fetch_one(&mut *tx)
    .await?;
    if !active {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "administrator token revoked",
        ));
    }
    let (epoch, head): (String, i64) =
        sqlx::query_as("SELECT epoch,head FROM event_publication_stream WHERE id")
            .fetch_one(&mut *tx)
            .await?;
    if epoch != cursor.epoch || cursor.sequence > head {
        return Err(expired());
    }
    let rows = sqlx::query("SELECT p.publication_seq,e.id AS source_id,e.job_id,e.event_type,e.payload_json FROM event_publications p LEFT JOIN job_events e ON e.id=p.event_id WHERE p.publication_seq>$1 AND p.publication_seq<=$2 ORDER BY p.publication_seq LIMIT 256")
        .bind(cursor.sequence).bind(head).fetch_all(&mut *tx).await?;
    let mut through = cursor.clone();
    let mut events = Vec::new();
    for row in &rows {
        let sequence: i64 = row.get("publication_seq");
        let source_id: Option<i64> = row.get("source_id");
        if through.sequence.checked_add(1) != Some(sequence) || source_id.is_none() {
            return Err(expired());
        }
        through.sequence = sequence;
        let job_id: String = row.get("job_id");
        if job.is_none_or(|id| id == job_id) {
            events.push(PersistedEvent {
                id: sequence,
                source_id: source_id.expect("checked source event"),
                job_id,
                kind: row.get("event_type"),
                data: row.get::<Value, _>("payload_json").to_string(),
            });
        }
    }
    if rows.len() < 256 && through.sequence < head {
        return Err(expired());
    }
    tx.commit().await?;
    Ok(Batch {
        more: through.sequence < head,
        through,
        events,
    })
}

pub(super) async fn events(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let cursor = request_cursor(&query, &headers)?;
    let first = batch(&state, &cursor, &actor.id, query.job_id.as_deref()).await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        stream(state, actor.id, query.job_id, cursor, first),
    )
        .into_response())
}
pub(super) async fn job_events(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if query.job_id.as_deref().is_some_and(|job| job != id) {
        return Err(ApiError::bad_request("conflicting job filters"));
    }
    let cursor = request_cursor(&query, &headers)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1)")
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;
    if !exists {
        return Err(ApiError::not_found("job"));
    }
    let first = batch(&state, &cursor, &actor.id, Some(&id)).await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        stream(state, actor.id, Some(id), cursor, first),
    )
        .into_response())
}

fn stream(
    state: AppState,
    actor: String,
    job: Option<String>,
    mut cursor: Cursor,
    first: Batch,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let output = stream! {
        let mut shutdown = state.shutdown.subscribe();
        let mut authorization = state.admin_tokens_changed.subscribe();
        let mut updates = state.events.subscribe(state.pool.clone());
        let mut first = Some(first);
        loop {
            if *shutdown.borrow() { break; }
            authorization.borrow_and_update();
            updates.borrow_and_update();
            let page = match first.take() {
                Some(page) => page,
                None => match batch(&state, &cursor, &actor, job.as_deref()).await {
                    Ok(page) => page,
                    Err(error) => {
                        if error.code == "event_cursor_expired" {
                            yield Ok(Event::default().event("stream.reset").data(json!({"code":error.code}).to_string()));
                        } else if error.status != StatusCode::UNAUTHORIZED {
                            tracing::warn!(%error,"v2 event stream read failed");
                        }
                        break;
                    }
                },
            };
            let mut delivered = cursor.sequence;
            for event in page.events {
                if *shutdown.borrow() { return; }
                delivered = event.id;
                let id = Cursor { epoch: cursor.epoch.clone(), sequence: event.id }.wire();
                yield Ok(Event::default().id(id).event(event.kind.clone()).data(event.wire_data()));
            }
            cursor = page.through;
            // Filtered streams still acknowledge the global rows they inspected.
            // Consumers save this checkpoint after processing prior business events.
            if delivered < cursor.sequence {
                yield Ok(Event::default().id(cursor.wire()).event("stream.cursor").data(json!({"cursor":cursor.wire()}).to_string()));
            }
            if page.more { continue; }
            tokio::select! {
                biased;
                _ = shutdown.wait_for(|stop| *stop) => break,
                _ = authorization.changed() => {},
                changed = updates.changed() => { if changed.is_err() { break; } },
                _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            }
        }
    };
    Sse::new(output).keep_alive(axum::response::sse::KeepAlive::default())
}

#[cfg(test)]
mod tests;
