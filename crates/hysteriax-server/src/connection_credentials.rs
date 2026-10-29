//! One connection password per user; node assignments contain authorization only.
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
