//! Probe signing prepares immutable material outside retryable write transactions.
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use super::ca::Authority;
use crate::{db, security::SecretBox};

#[derive(Clone)]
pub(crate) struct PreparedProbe {
    node: String,
    ca: String,
    ca_version: i64,
    node_revision: i64,
    generation: String,
    previous: Option<(String, i64)>,
    new_credential: String,
    payload_enc: String,
    metadata: Value,
    expires_at: DateTime<Utc>,
    next_action_at: DateTime<Utc>,
}

impl PreparedProbe {
    pub(crate) fn matches_scope(
        &self,
        node: &str,
        ca: &str,
        version: i64,
        credential: &str,
        credential_version: i64,
    ) -> bool {
        self.node == node
            && self.ca == ca
            && self.ca_version == version
            && self
                .previous
                .as_ref()
                .is_some_and(|(id, v)| id == credential && *v == credential_version)
    }
}

pub(crate) async fn prepare(
    pool: &PgPool,
    secrets: &SecretBox,
    node: &str,
    ca: &str,
    ca_version: i64,
) -> Result<PreparedProbe> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let now = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await?;
    let row=sqlx::query("SELECT n.desired_revision,n.state AS node_state,g.state,c.archived FROM nodes n CROSS JOIN mtls_ca_generations g JOIN credentials c ON c.id=g.ca_id WHERE n.id=$1 AND g.ca_id=$2 AND g.ca_version=$3")
        .bind(node).bind(ca).bind(ca_version).fetch_one(&mut *tx).await?;
    let generation: String = row.get("state");
    ensure!(generation != "retired", "retired CA cannot sign a probe");
    ensure!(!row.get::<bool, _>("archived"), "CA identity is archived");
    ensure!(
        !matches!(
            row.get::<String, _>("node_state").as_str(),
            "deleting" | "delete_failed"
        ),
        "node is being deleted"
    );
    let previous:Option<(String,i64)>=sqlx::query_as("SELECT p.credential_id,p.credential_version FROM mtls_probe_versions p WHERE node_id=$1 AND ca_id=$2 AND ca_version=$3 AND NOT discarded ORDER BY credential_version DESC LIMIT 1")
        .bind(node).bind(ca).bind(ca_version).fetch_optional(&mut *tx).await?;
    let cipher: String = sqlx::query_scalar(
        "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=$2",
    )
    .bind(ca)
    .bind(ca_version)
    .fetch_one(&mut *tx)
    .await?;
    let payload: Value = serde_json::from_str(&secrets.decrypt(&cipher)?)?;
    let expected = row.get("desired_revision");
    tx.commit().await?;
    let node_id = Uuid::parse_str(node)?;
    let issued = tokio::task::spawn_blocking(move || {
        Authority::from_payload(&payload, now)?.sign_probe(&node_id, now)
    })
    .await??;
    let payload = issued.material.payload();
    let mut metadata = super::validate_payload("tls_identity", &payload)?;
    metadata["managed"] = json!(true);
    metadata["internal_probe"] = json!(true);
    metadata["ca_id"] = json!(ca);
    metadata["node_id"] = json!(node);
    metadata["ca_version"] = json!(ca_version);
    Ok(PreparedProbe {
        node: node.into(),
        ca: ca.into(),
        ca_version,
        node_revision: expected,
        generation,
        previous,
        new_credential: Uuid::new_v4().to_string(),
        payload_enc: secrets.encrypt(&payload.to_string())?,
        metadata,
        expires_at: issued.expires_at,
        next_action_at: issued.next_action_at,
    })
}

pub(crate) async fn commit(pool: &PgPool, prepared: PreparedProbe) -> Result<(String, i64)> {
    db::serializable::<_, anyhow::Error, _>(pool, move |tx| {
        let p = prepared.clone();
        Box::pin(async move { commit_on(tx, p).await })
    })
    .await
}

pub(crate) async fn commit_on(
    tx: &mut Transaction<'_, Postgres>,
    p: PreparedProbe,
) -> Result<(String, i64)> {
    let node: Option<(i64, String)> =
        sqlx::query_as("SELECT desired_revision,state FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
            .bind(&p.node)
            .fetch_optional(&mut **tx)
            .await?;
    ensure!(
        node.as_ref()
            .is_some_and(|(revision, state)| *revision == p.node_revision
                && !matches!(state.as_str(), "deleting" | "delete_failed")),
        "node or CA changed during probe signing"
    );
    let mut ids = vec![p.ca.clone()];
    if let Some((credential, _)) = &p.previous {
        ids.push(credential.clone());
    }
    ids.sort();
    ids.dedup();
    sqlx::query(
        "SELECT id FROM credentials WHERE id=ANY($1::text[]) ORDER BY id FOR NO KEY UPDATE",
    )
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await?;
    let generation:Option<(String,bool)>=sqlx::query_as("SELECT g.state,c.archived FROM mtls_ca_generations g JOIN credentials c ON c.id=g.ca_id WHERE g.ca_id=$1 AND g.ca_version=$2 FOR SHARE OF g")
                .bind(&p.ca).bind(p.ca_version).fetch_optional(&mut **tx).await?;
    ensure!(
        generation == Some((p.generation.clone(), false)),
        "node or CA changed during probe signing"
    );
    let current:Option<(String,i64)>=sqlx::query_as("SELECT credential_id,credential_version FROM mtls_probe_versions WHERE node_id=$1 AND ca_id=$2 AND ca_version=$3 AND NOT discarded ORDER BY credential_version DESC LIMIT 1")
                .bind(&p.node).bind(&p.ca).bind(p.ca_version).fetch_optional(&mut **tx).await?;
    ensure!(
        current == p.previous,
        "probe identity changed during signing"
    );
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    ensure!(
        p.expires_at > now,
        "prepared probe certificate expired; retry"
    );
    let (credential, version) = if let Some((credential, version)) = &p.previous {
        let previous:DateTime<Utc>=sqlx::query_scalar("SELECT expires_at FROM mtls_probe_versions WHERE node_id=$1 AND credential_id=$2 AND credential_version=$3")
                    .bind(&p.node).bind(credential).bind(version).fetch_one(&mut **tx).await?;
        ensure!(
            p.expires_at > previous,
            "CA update required: cannot extend probe certificate"
        );
        let next: i64 = sqlx::query_scalar("SELECT latest_version+1 FROM credentials WHERE id=$1")
            .bind(credential)
            .fetch_one(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES($1,$2,$3,$4,$5)")
                    .bind(credential).bind(next).bind(&p.payload_enc).bind(&p.metadata).bind(now).execute(&mut **tx).await?;
        sqlx::query("UPDATE credentials SET latest_version=$2,revision=revision+1,updated_at=$3 WHERE id=$1")
                    .bind(credential).bind(next).bind(now).execute(&mut **tx).await?;
        (credential.clone(), next)
    } else {
        sqlx::query("INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES($1,$2,'tls_identity',$3,$3)")
                    .bind(&p.new_credential).bind(format!("Internal probe · {}",p.node)).bind(now).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES($1,1,$2,$3,$4)")
                    .bind(&p.new_credential).bind(&p.payload_enc).bind(&p.metadata).bind(now).execute(&mut **tx).await?;
        (p.new_credential.clone(), 1)
    };
    sqlx::query("INSERT INTO mtls_probe_versions(node_id,credential_id,credential_version,ca_id,ca_version,expires_at,next_action_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(&p.node).bind(&credential).bind(version).bind(&p.ca).bind(p.ca_version).bind(p.expires_at).bind(p.next_action_at).execute(&mut **tx).await?;
    if p.generation == "published" {
        sqlx::query("UPDATE nodes SET mtls_error=NULL WHERE id=$1 AND mtls_state='ready' AND mtls_error LIKE 'Probe certificate renewal failed:%'")
                    .bind(&p.node).execute(&mut **tx).await?;
        super::rotations::switch_probe_on(tx, &p.node, &p.ca, p.ca_version).await?;
    }
    Ok((credential, version))
}

#[cfg(test)]
mod tests;
