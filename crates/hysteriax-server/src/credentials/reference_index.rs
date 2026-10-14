//! Encrypted configurations are indexed once per change, under the existing write lock.
//! Every authoritative reference check drains committed dirty sources in its transaction.
use crate::{db, error::ApiError, security::SecretBox};
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row};

pub async fn ensure_on(connection: &mut PgConnection, secrets: &SecretBox) -> Result<(), ApiError> {
    sqlx::query("SELECT pg_advisory_xact_lock(62177345902811)")
        .execute(&mut *connection)
        .await?;
    let dirty = sqlx::query(
        "SELECT kind,source_id FROM credential_reference_dirty ORDER BY kind,source_id",
    )
    .fetch_all(&mut *connection)
    .await?;
    for source in dirty {
        let kind: String = source.get("kind");
        let id: String = source.get("source_id");
        sqlx::query("DELETE FROM credential_reference_edges WHERE kind=$1 AND source_id=$2")
            .bind(&kind)
            .bind(&id)
            .execute(&mut *connection)
            .await?;
        if kind == "node" {
            if let Some(row) = sqlx::query(
                "SELECT id,desired_config_enc,deployed_config_enc FROM nodes WHERE id=$1",
            )
            .bind(&id)
            .fetch_optional(&mut *connection)
            .await?
            {
                for (column, source) in [
                    ("desired_config_enc", "desired"),
                    ("deployed_config_enc", "deployed"),
                ] {
                    if let Some(cipher) = row.get::<Option<String>, _>(column) {
                        insert(
                            connection, secrets, &kind, &id, &id, None, None, source, &cipher,
                        )
                        .await?;
                    }
                }
            }
        } else if let Some(row) =
            sqlx::query("SELECT node_id,revision,config_enc FROM config_versions WHERE id=$1")
                .bind(&id)
                .fetch_optional(&mut *connection)
                .await?
        {
            insert(
                connection,
                secrets,
                &kind,
                &id,
                &row.get::<String, _>("node_id"),
                Some(&id),
                Some(row.get("revision")),
                "history",
                &row.get::<String, _>("config_enc"),
            )
            .await?;
        }
        sqlx::query("DELETE FROM credential_reference_dirty WHERE kind=$1 AND source_id=$2")
            .bind(&kind)
            .bind(&id)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn insert(
    connection: &mut PgConnection,
    secrets: &SecretBox,
    kind: &str,
    id: &str,
    node: &str,
    version_id: Option<&str>,
    revision: Option<i64>,
    source: &str,
    cipher: &str,
) -> Result<(), ApiError> {
    let config: Value =
        serde_json::from_str(&secrets.decrypt(cipher)?).map_err(|_| ApiError::internal())?;
    let references = super::references(&config).map_err(|_| ApiError::internal())?;
    if references.is_empty() {
        return Ok(());
    }
    let fields: Vec<_> = references.iter().map(|(field, _)| field.clone()).collect();
    let ids: Vec<_> = references
        .iter()
        .map(|(_, reference)| reference.id.clone())
        .collect();
    let versions: Vec<_> = references
        .iter()
        .map(|(_, reference)| reference.version)
        .collect();
    sqlx::query("INSERT INTO credential_reference_edges(kind,source_id,node_id,config_version_id,config_revision,source,field,credential_id,version) SELECT $1,$2,$3,$4,$5,$6,field,credential_id,version FROM unnest($7::text[],$8::text[],$9::bigint[]) AS refs(field,credential_id,version)")
        .bind(kind).bind(id).bind(node).bind(version_id).bind(revision).bind(source).bind(fields).bind(ids).bind(versions).execute(connection).await?;
    Ok(())
}
pub async fn initialize(pool: &PgPool, secrets: &SecretBox) -> Result<(), ApiError> {
    let mut tx = db::begin_write(pool).await?;
    ensure_on(&mut tx, secrets).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json,
        extract::{Extension, Path, Query, State},
    };
    use serde_json::json;
    #[tokio::test]
    async fn changed_and_historical_references_block_deletion_without_prior_index_refresh() {
        let state = crate::state::AppState::new(
            crate::db::test_pool().await,
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
        );
        let mut ids = Vec::new();
        for name in ["First", "Second"] {
            let mut tx = db::begin_write(&state.pool).await.unwrap();
            let id = super::super::insert(
                &mut tx,
                &state.secrets,
                name,
                "ssh_password",
                None,
                &json!({"secret":"test"}),
                &json!({}),
                &json!({}),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            ids.push(id);
        }
        let node=crate::credentials::test_node_request(&state,json!({"name":"Node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"test","public_host":"example.test","public_port":443,"listen_addr":":443"})).await;
        let (_, Json(created)) = crate::api::nodes::create(State(state.clone()), Json(node))
            .await
            .unwrap();
        let node = created["node"]["id"].as_str().unwrap();
        for id in &ids {
            let cipher = state
                .secrets
                .encrypt(
                    &json!({"test_reference":format!("credential://{id}/1/secret")}).to_string(),
                )
                .unwrap();
            sqlx::query("UPDATE nodes SET desired_config_enc=$1 WHERE id=$2")
                .bind(&cipher)
                .bind(node)
                .execute(&state.pool)
                .await
                .unwrap();
            let error = crate::api::credentials::delete(
                State(state.clone()),
                Extension(crate::api::AdminActor { id: "test".into() }),
                Path(id.clone()),
                Query(crate::api::nodes::RevisionQuery {
                    expected_revision: Some(1),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
        }
        let first = crate::api::credentials::find_references(&state, &ids[0])
            .await
            .unwrap();
        assert!(first.is_empty());
        let history = state
            .secrets
            .encrypt(
                &json!({"old_reference":format!("credential://{}/1/secret",ids[0])}).to_string(),
            )
            .unwrap();
        sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('history-test',$1,2,$2,'hash',now())").bind(node).bind(history).execute(&state.pool).await.unwrap();
        let references = crate::api::credentials::find_references(&state, &ids[0])
            .await
            .unwrap();
        assert!(references.iter().any(|r| r["source"] == "history"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credential_reference_dirty")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            0
        );
    }
}
