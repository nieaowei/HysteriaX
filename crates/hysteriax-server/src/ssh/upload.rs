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
#[path = "upload_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "upload_openssh_tests.rs"]
mod openssh_tests;
