//! User-owned connection and subscription secrets, issued and rotated together.
use crate::{
    api::generate_token,
    security::{SecretBox, token_digest},
};
use anyhow::Result;
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

/// Prepared before entering retryable transactions; retries reuse both secrets.
#[derive(Clone)]
pub struct CredentialPair {
    pub connection: Credential,
    pub subscription: Credential,
    connection_id: String,
    subscription_id: String,
}

impl CredentialPair {
    pub fn new(secrets: &SecretBox) -> Result<Self> {
        Ok(Self {
            connection: Credential::new(secrets)?,
            subscription: Credential::new(secrets)?,
            connection_id: uuid::Uuid::new_v4().to_string(),
            subscription_id: uuid::Uuid::new_v4().to_string(),
        })
    }

    pub async fn insert_on(
        &self,
        tx: &mut sqlx::PgConnection,
        user: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at) VALUES($1,$2,'connection',$3,$4,$5),($6,$2,'subscription',$7,$8,$5)")
            .bind(&self.connection_id).bind(user).bind(&self.connection.hash).bind(&self.connection.ciphertext).bind(at)
            .bind(&self.subscription_id).bind(&self.subscription.hash).bind(&self.subscription.ciphertext)
            .execute(tx).await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "user_credentials_tests.rs"]
mod tests;
