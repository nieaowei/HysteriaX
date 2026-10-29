//! One connection password per user; node assignments contain authorization only.
use crate::{
    api::generate_token,
    security::{SecretBox, token_digest},
};
use anyhow::Result;
use sqlx::{PgPool, Row};
#[derive(Clone)]
pub struct Credential {
    pub plaintext: String,
    pub hash: String,
    pub ciphertext: String,
}
impl Credential {
    pub fn new(secrets: &SecretBox) -> Result<Self> {
        let plaintext = generate_token();
        Ok(Self {
            hash: token_digest(&plaintext),
            ciphertext: secrets.encrypt(&plaintext)?,
            plaintext,
        })
    }
}
/// Used during startup and by the standalone test-environment migration command.
/// A committed batch is restart-safe; passwords are never printed or overwritten.
pub async fn migrate(pool: &PgPool, secrets: &SecretBox) -> Result<()> {
    loop {
        let mut tx = pool.begin().await?;
        let rows=sqlx::query("SELECT id FROM users WHERE connection_credential_hash IS NULL OR connection_credential_enc IS NULL ORDER BY id LIMIT 256 FOR UPDATE SKIP LOCKED").fetch_all(&mut *tx).await?;
        if rows.is_empty() {
            let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE connection_credential_hash IS NULL OR connection_credential_enc IS NULL)").fetch_one(&mut *tx).await?;
            tx.commit().await?;
            if !pending {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            continue;
        }
        for row in rows {
            let credential = Credential::new(secrets)?;
            sqlx::query("UPDATE users SET connection_credential_hash=$2,connection_credential_enc=$3,updated_at=clock_timestamp() WHERE id=$1").bind(row.get::<String,_>("id")).bind(credential.hash).bind(credential.ciphertext).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
}
