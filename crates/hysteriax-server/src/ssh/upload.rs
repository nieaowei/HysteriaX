//! Upload payloads use the same receiver-side fence and completion journal as
//! commands. The bytes travel on stdin, never in argv or persisted metadata.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) struct Transfer {
    pub command: String,
    pub verification: Value,
}

impl Transfer {
    pub fn new(path: &str, contents: &[u8], timeout: u64) -> Result<Self> {
        ensure!(
            path.starts_with('/') && path.len() <= 4096 && !path.contains('\0'),
            "invalid upload destination"
        );
        ensure!((1..=3600).contains(&timeout), "invalid upload timeout");
        let digest = hex::encode(Sha256::digest(contents));
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let script = format!(
            r#"set -eu
umask 077
target={}
[ ! -d "$target" ] && [ ! -L "$target" ] || exit 1
temporary=$(mktemp "$target.upload.XXXXXXXXXX")
trap 'rm -f -- "$temporary"' EXIT
trap 'exit 1' HUP INT TERM
head -c {} > "$temporary"
[ "$(wc -c < "$temporary")" -eq {} ] || exit 1
actual=$(sha256sum "$temporary"); actual=${{actual%% *}}
[ "$actual" = {} ] || exit 1
mv -fT -- "$temporary" "$target"
"#,
            quote(path),
            contents
                .len()
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("upload too large"))?,
            contents.len(),
            quote(&digest)
        );
        // Bound even an orphaned receiver when the SSH transport disappears.
        let command = format!(
            "timeout --kill-after=2s {timeout}s sh -c {}",
            quote(&script)
        );
        Ok(Self {
            command,
            verification: json!({"remote_path":path,"content_sha256":digest,"content_bytes":contents.len()}),
        })
    }
}

/// Reuse only the exact pinned release; verify the copied bytes again before
/// publishing the staging file. No installed file is modified by this command.
pub(super) fn reuse_installed_binary(path: &str, contents: &[u8]) -> Result<String> {
    ensure!(
        path.starts_with('/') && path.len() <= 4096 && !path.contains('\0'),
        "invalid upload destination"
    );
    let digest = hex::encode(Sha256::digest(contents));
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    Ok(format!(
        r#"set -eu
umask 077
source=/opt/hysteriax/hysteria
target={}
expected={}
if [ ! -f "$source" ] || [ -L "$source" ]; then printf 'upload\n'; exit 0; fi
actual=$(sha256sum "$source"); actual=${{actual%% *}}
if [ "$actual" != "$expected" ]; then printf 'upload\n'; exit 0; fi
temporary=$(mktemp "$target.reuse.XXXXXXXXXX")
trap 'rm -f -- "$temporary"' EXIT
cp -- "$source" "$temporary"
actual=$(sha256sum "$temporary"); actual=${{actual%% *}}
[ "$actual" = "$expected" ] || exit 1
chmod 0600 "$temporary"
mv -fT -- "$temporary" "$target"
printf 'reused\n'
"#,
        quote(path),
        quote(&digest)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upload_metadata_binds_destination_size_and_hash_without_payload() {
        let transfer = Transfer::new("/tmp/a'b", b"private file contents", 120).unwrap();
        assert_eq!(transfer.verification["remote_path"], "/tmp/a'b");
        assert_eq!(transfer.verification["content_bytes"], 21);
        assert_eq!(
            transfer.verification["content_sha256"],
            hex::encode(Sha256::digest(b"private file contents"))
        );
        assert!(!transfer.command.contains("private file contents"));
        assert!(Transfer::new("relative", b"", 120).is_err());
        assert!(Transfer::new("/tmp/a\0b", b"", 120).is_err());
        assert!(Transfer::new("/tmp/a", b"", 0).is_err());
    }
}

#[cfg(test)]
mod openssh_tests {
    use super::*;
    use crate::ssh::{self, FingerprintResult, SshNode};

    #[tokio::test]
    #[ignore = "requires an owned OpenSSH fixture and optional bandwidth-limited TCP forwarder"]
    async fn real_openssh_upload_streams_a_release_sized_file_with_independent_deadline() {
        let node = SshNode {
            host: "127.0.0.1".into(),
            port: std::env::var("TEST_SSH_UPLOAD_PORT")
                .unwrap()
                .parse()
                .unwrap(),
            username: "root".into(),
            auth_type: "password".into(),
            secret: std::env::var("TEST_SSH_UPLOAD_SECRET")
                .unwrap_or_else(|_| "fixture-upload-only".into()),
            passphrase: None,
            host_fingerprint: Some(std::env::var("TEST_SSH_UPLOAD_FINGERPRINT").unwrap()),
        };
        let FingerprintResult::Trusted(session) = ssh::connect(&node).await.unwrap() else {
            panic!("fixture host identity must be pinned");
        };
        let size = std::env::var("TEST_SSH_UPLOAD_BYTES")
            .ok()
            .map(|v| v.parse::<usize>().unwrap())
            .unwrap_or(23_003_298);
        let payload: Vec<u8> = match std::env::var_os("TEST_SSH_UPLOAD_FILE") {
            Some(path) => std::fs::read(path).unwrap(),
            None => (0..size).map(|i| ((i * 31) % 251) as u8).collect(),
        };
        let path = format!("/tmp/hx-upload-openssh-{}", uuid::Uuid::new_v4());
        let expected = hex::encode(Sha256::digest(&payload));
        let pool = crate::db::test_pool_with_max_connections(3).await;
        let mut tx = pool.begin().await.unwrap();
        let resource = format!("node:openssh-upload-{}", uuid::Uuid::new_v4());
        let lease = crate::execution::acquire(&mut tx, &resource, "fixture", "upload")
            .await
            .unwrap()
            .unwrap();
        tx.commit().await.unwrap();
        let transfer = async {
            if std::env::var_os("TEST_SSH_UPLOAD_REUSE").is_some() {
                session.upload_release_binary(&path, &payload).await
            } else {
                session.upload(&path, &payload).await
            }
        };
        crate::execution::supervise(pool, vec![lease], transfer)
            .await
            .unwrap()
            .unwrap();
        let actual = session
            .execute_read_only_checked(&format!("sha256sum {path}"))
            .await
            .unwrap();
        assert_eq!(actual.split_whitespace().next(), Some(expected.as_str()));
        session
            .execute_checked(&format!("rm -- {path}"))
            .await
            .unwrap();
    }
}
