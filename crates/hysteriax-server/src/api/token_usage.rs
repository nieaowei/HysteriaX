use crate::db;
use sqlx::PgPool;

pub(super) fn schedule(state: &crate::state::AppState, id: &str, digest: &str) {
    // One in-flight update per instance; never queue a task per incoming request.
    let Ok(permit) = state.admin_token_usage_updates.clone().try_acquire_owned() else {
        return;
    };
    let (pool, id, digest) = (state.pool.clone(), id.to_owned(), digest.to_owned());
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(error) = touch(&pool, &id, &digest).await
            && error.as_database_error().and_then(|e| e.code()).as_deref() != Some("55P03")
        {
            tracing::warn!(error = %error, "administrator token usage timestamp not updated");
        }
    });
}

pub(super) async fn touch(pool: &PgPool, id: &str, digest: &str) -> Result<(), sqlx::Error> {
    let (id, digest) = (id.to_owned(), digest.to_owned());
    db::write(pool,move |tx|{let (id,digest)=(id.clone(),digest.clone());Box::pin(async move {
        // Optional metadata also gives up quickly on dependent cache-row locks.
        sqlx::query("SELECT set_config('lock_timeout','1ms',true),set_config('statement_timeout','2s',true)").execute(&mut **tx).await?;
        let row=sqlx::query("SELECT id FROM admin_tokens WHERE id=$1 AND token_hash=$2 AND revoked_at IS NULL AND (last_used_at IS NULL OR last_used_at<=clock_timestamp()-interval '60 seconds') FOR UPDATE SKIP LOCKED")
            .bind(&id).bind(&digest).fetch_optional(&mut **tx).await?;
        if row.is_some() {
            sqlx::query("UPDATE admin_tokens SET last_used_at=clock_timestamp() WHERE id=$1 AND token_hash=$2 AND revoked_at IS NULL")
                .bind(id).bind(digest).execute(&mut **tx).await?;
        }
        Ok(())
    })}).await
}

#[cfg(test)]
mod tests;
