use crate::db;
use sqlx::PgPool;

pub(super) async fn touch(pool: &PgPool, id: &str, digest: &str) -> Result<(), sqlx::Error> {
    let (id, digest) = (id.to_owned(), digest.to_owned());
    db::write(pool,move |tx|{let (id,digest)=(id.clone(),digest.clone());Box::pin(async move {
        // Optional metadata also gives up quickly on dependent cache-row locks.
        sqlx::query("SET LOCAL lock_timeout='1ms'").execute(&mut **tx).await?;
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
