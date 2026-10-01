use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, OsRng, rand_core::RngCore},
};

#[derive(Clone)]
pub struct SecretBox(XChaCha20Poly1305);

impl SecretBox {
    pub fn from_base64(encoded: &str) -> Result<Self> {
        let key = STANDARD_NO_PAD
            .decode(encoded)
            .context("decode HYSTERIAX_MASTER_KEY")?;
        if key.len() != 32 {
            bail!("HYSTERIAX_MASTER_KEY must decode to exactly 32 bytes");
        }
        Ok(Self(
            XChaCha20Poly1305::new_from_slice(&key).expect("validated key length"),
        ))
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        self.encrypt_bytes(plaintext.as_bytes())
    }

    pub fn encrypt_bytes(&self, plaintext: &[u8]) -> Result<String> {
        let mut nonce = [0_u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let encrypted = self
            .0
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .map_err(|_| anyhow::anyhow!("encrypt secret"))?;
        let mut payload = nonce.to_vec();
        payload.extend(encrypted);
        Ok(STANDARD_NO_PAD.encode(payload))
    }

    pub fn decrypt(&self, ciphertext: &str) -> Result<String> {
        String::from_utf8(self.decrypt_bytes(ciphertext)?).context("secret is not valid UTF-8")
    }

    pub fn decrypt_bytes(&self, ciphertext: &str) -> Result<Vec<u8>> {
        let payload = STANDARD_NO_PAD
            .decode(ciphertext)
            .context("decode encrypted secret")?;
        if payload.len() < 40 {
            bail!("encrypted secret is truncated");
        }
        let (nonce, encrypted) = payload.split_at(24);
        self.0
            .decrypt(XNonce::from_slice(nonce), encrypted)
            .map_err(|_| anyhow::anyhow!("decrypt secret"))
    }
}

#[cfg(test)]
mod tests {
    use super::SecretBox;

    #[test]
    fn encrypts_and_authenticates_secret_values() {
        let secrets =
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let first = secrets.encrypt("example-private-key").unwrap();
        let second = secrets.encrypt("example-private-key").unwrap();
        assert_ne!(first, second);
        assert_eq!(secrets.decrypt(&first).unwrap(), "example-private-key");
        assert!(secrets.decrypt("invalid-ciphertext").is_err());

        let binary = [0_u8, 255, 64, 13, 10];
        let encrypted = secrets.encrypt_bytes(&binary).unwrap();
        assert_eq!(secrets.decrypt_bytes(&encrypted).unwrap(), binary);
    }
}
