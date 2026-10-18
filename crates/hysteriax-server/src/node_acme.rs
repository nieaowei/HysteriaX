//! Node-local certificate policy compilation. Public configuration remains ACME.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const VERSION: &str = "0.1.0";

pub fn component_config(options: &Value) -> Result<Option<Value>> {
    let Some(acme) = options.get("acme") else {
        return Ok(None);
    };
    crate::config::validate_acme(acme)?;
    let id = hex::encode(Sha256::digest(serde_json::to_vec(
        &json!({"version": VERSION, "acme": acme}),
    )?));
    let mut acme = acme.clone();
    let dir = acme["dir"]
        .as_str()
        .filter(|d| !d.is_empty())
        .unwrap_or("/var/lib/hysteriax/acme");
    let dir = if std::path::Path::new(dir).is_absolute() {
        dir.to_owned()
    } else {
        format!("/var/lib/hysteriax/{dir}")
    };
    ensure!(
        dir.starts_with('/')
            && !dir.split('/').any(|s| s == ".." || s == ".")
            && !dir.contains(['\n', '\r', '\0']),
        "invalid ACME state directory"
    );
    acme["dir"] = json!(dir);
    Ok(Some(
        json!({"policy_id":id,"state_dir":format!("/var/lib/hysteriax-acme/{id}"),"publish_dir":format!("/var/lib/hysteriax/certificates/{id}"),"acme":acme}),
    ))
}

pub fn compile(options: &Value) -> Result<Value> {
    let mut compiled = options.clone();
    if let Some(component) = component_config(options)? {
        let dir = component["publish_dir"]
            .as_str()
            .context("missing publication directory")?;
        let object = compiled
            .as_object_mut()
            .context("invalid server configuration")?;
        ensure!(
            !object.contains_key("tls"),
            "tls and acme are mutually exclusive"
        );
        object.remove("acme");
        object.insert("tls".into(), json!({"cert":format!("{dir}/current/cert.pem"),"key":format!("{dir}/current/key.pem")}));
    }
    Ok(compiled)
}

pub async fn artifact(architecture: &str) -> Result<Vec<u8>> {
    ensure!(
        matches!(architecture, "amd64" | "arm64"),
        "unsupported ACME architecture"
    );
    load_artifact(&format!("hysteriax-acme-linux-{architecture}")).await
}

pub async fn licenses() -> Result<Vec<u8>> {
    load_artifact("LICENSES.tar").await
}

async fn load_artifact(name: &str) -> Result<Vec<u8>> {
    let root = std::env::var("HYSTERIAX_ACME_ARTIFACT_DIR")
        .unwrap_or_else(|_| "/usr/local/share/hysteriax/acme".into());
    let manifest = tokio::fs::read_to_string(format!("{root}/SHA256SUMS"))
        .await
        .context("ACME artifact manifest is missing; rebuild the management image")?;
    let expected = manifest
        .lines()
        .find_map(|l| {
            let mut fields = l.split_whitespace();
            let hash = fields.next()?;
            (fields.next()? == name).then_some(hash)
        })
        .context("ACME artifact checksum missing")?;
    let bytes = tokio::fs::read(format!("{root}/{name}")).await?;
    ensure!(
        hex::encode(Sha256::digest(&bytes)) == expected,
        "ACME artifact checksum mismatch"
    );
    Ok(bytes)
}

/// Collect before querying Hy2 so stopped/activating nodes retain certificate visibility.
/// Compare the deployed digest again on write: an old SSH result cannot overwrite a new deployment.
pub async fn sample(
    pool: &sqlx::PgPool,
    secrets: &crate::security::SecretBox,
    node: &str,
    session: &crate::ssh::SshSession,
) -> Result<()> {
    use sqlx::Row;
    let row = sqlx::query("SELECT deployed_revision,deployed_config_enc,deployed_content_sha256 FROM nodes WHERE id=$1").bind(node).fetch_one(pool).await?;
    let Some(cipher) = row.get::<Option<String>, _>("deployed_config_enc") else {
        return Ok(());
    };
    let public: Value = serde_json::from_str(&secrets.decrypt(&cipher)?)?;
    if public.get("acme").is_none() {
        return Ok(());
    }
    let (resolved, _) = crate::credentials::resolve_config(pool, secrets, &public, false).await?;
    let expected = component_config(&resolved)?.context("missing ACME policy")?;
    let command = "/opt/hysteriax/hysteriax-acme status /etc/hysteriax/acme.json";
    // status requires the private config; sudo uses a fixed command without secrets.
    let output = session.execute(command).await?;
    let output = if output.code == 0 {
        output.stdout
    } else {
        session
            .execute_checked(&format!("sudo -n {command}"))
            .await?
    };
    ensure!(output.len() <= 65536, "ACME status response is too large");
    let mut status: Value = serde_json::from_str(&output)?;
    ensure!(
        status["schema_version"] == 1
            && status["policy_id"] == expected["policy_id"]
            && status["domains"] == expected["acme"]["domains"],
        "ACME status policy mismatch"
    );
    let allowed = [
        "schema_version",
        "component_version",
        "policy_id",
        "state",
        "domains",
        "fingerprint",
        "not_before",
        "not_after",
        "last_success",
        "next_attempt",
        "error",
        "failures",
        "collected_at",
    ];
    status
        .as_object_mut()
        .context("invalid ACME status")?
        .retain(|key, _| allowed.contains(&key.as_str()));
    // Do not trust arbitrary remote error strings with a public API or UI.
    if status
        .get("error")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        status["error"] = json!(
            "Certificate maintenance failed. Check validation, challenge ports, DNS credentials and CA availability."
        );
    }
    let service = session
        .execute("systemctl is-active hysteriax-acme.service")
        .await?;
    status["service_state"] = json!(match service.stdout.trim() {
        "active" => "active",
        "activating" => "activating",
        "failed" => "failed",
        _ => "inactive",
    });
    let now = chrono::Utc::now();
    status["collected_at"] = json!(now);
    let revision: Option<i64> = row.get("deployed_revision");
    let digest: Option<String> = row.get("deployed_content_sha256");
    sqlx::query("INSERT INTO node_acme_observations(node_id,revision,deployment_digest,policy_id,status,sampled_at) SELECT id,$2,$3,$4,$5,$6 FROM nodes WHERE id=$1 AND deployed_revision=$2 AND deployed_content_sha256=$3 AND state NOT IN ('deleting','delete_failed') ON CONFLICT(node_id) DO UPDATE SET revision=excluded.revision,deployment_digest=excluded.deployment_digest,policy_id=excluded.policy_id,status=excluded.status,sampled_at=excluded.sampled_at WHERE node_acme_observations.sampled_at<excluded.sampled_at")
        .bind(node).bind(revision).bind(digest).bind(expected["policy_id"].as_str()).bind(status).bind(now).execute(pool).await?;
    Ok(())
}

pub async fn observed(pool: &sqlx::PgPool, node: &str) -> Result<Option<Value>> {
    Ok(sqlx::query_scalar("SELECT o.status FROM node_acme_observations o JOIN nodes n ON n.id=o.node_id AND n.deployed_revision=o.revision AND n.deployed_content_sha256=o.deployment_digest WHERE n.id=$1").bind(node).fetch_optional(pool).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiles_san_acme_before_mtls_without_mutating_public_config() {
        let public = json!({"acme":{"domains":["a.example","b.example"]},"mtls":{"mode":"off"}});
        let c = compile(&public).unwrap();
        assert!(c.get("acme").is_none());
        assert!(
            c["tls"]["cert"]
                .as_str()
                .unwrap()
                .ends_with("/current/cert.pem")
        );
        assert!(public.get("acme").is_some());
        assert_eq!(c, compile(&public).unwrap());
        assert_ne!(
            c,
            compile(&json!({"acme":{"domains":["c.example"]}})).unwrap()
        );
        assert!(compile(&json!({"acme":{"domains":["a.example"]},"tls":{}})).is_err());
    }
    #[test]
    fn preserves_relative_directory_semantics_and_rejects_parent_traversal() {
        assert_eq!(
            component_config(&json!({"acme":{"domains":["a.example"],"dir":"custom"}}))
                .unwrap()
                .unwrap()["acme"]["dir"],
            "/var/lib/hysteriax/custom"
        );
        assert!(
            component_config(&json!({"acme":{"domains":["a.example"],"dir":"../other"}})).is_err()
        );
    }
}
