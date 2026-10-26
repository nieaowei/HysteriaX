//! Configuration references are committed with their encrypted source.
//! Only explicit maintenance imports may leave a dirty source between commits.
use crate::{db, error::ApiError, security::SecretBox};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};

pub async fn begin_read(
    pool: &PgPool,
    _secrets: &SecretBox,
) -> Result<crate::read_snapshot::Reader, ApiError> {
    if let Some(reader) = crate::read_snapshot::current().await {
        return Ok(reader);
    }
    Ok(crate::read_snapshot::Reader::owned(
        begin_read_transaction(pool, _secrets).await?,
    ))
}
pub(crate) async fn begin_read_transaction(
    pool: &PgPool,
    _secrets: &SecretBox,
) -> Result<Transaction<'static, Postgres>, ApiError> {
    let mut tx = crate::read_snapshot::transaction(pool).await?;
    assert_clean_on(&mut tx).await?;
    Ok(tx)
}
pub async fn assert_clean_on(connection: &mut PgConnection) -> Result<(), ApiError> {
    let dirty: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_reference_dirty)")
        .fetch_one(connection)
        .await?;
    if dirty {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "reference_initialization_required",
            "configuration reference initialization is incomplete; run maintenance migration",
        ));
    }
    Ok(())
}
pub async fn assert_ready(pool: &PgPool) -> Result<(), ApiError> {
    let mut connection = pool.acquire().await?;
    assert_clean_on(&mut connection).await?;
    let validated:i64=sqlx::query_scalar("SELECT count(*) FROM pg_constraint WHERE conrelid='credential_reference_edges'::regclass AND conname IN ('reference_edge_shape','reference_edge_version','reference_edge_history') AND convalidated").fetch_one(&mut *connection).await?;
    if validated != 3 {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "reference_initialization_required",
            "configuration reference constraints are not validated; run maintenance migration",
        ));
    }
    Ok(())
}

/// Flush exactly the changed node and history sources owned by this mutation.
/// The caller owns the node row before changing its configurations.
pub async fn flush_node_on(
    connection: &mut PgConnection,
    secrets: &SecretBox,
    node: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(node)
        .fetch_optional(&mut *connection)
        .await?;
    let sources:Vec<(String,String,i64)>=sqlx::query_as("SELECT d.kind,d.source_id,d.generation FROM credential_reference_dirty d LEFT JOIN config_versions v ON d.kind='history' AND v.id=d.source_id WHERE (d.kind='node' AND d.source_id=$1) OR (d.kind='history' AND v.node_id=$1) ORDER BY d.kind,d.source_id FOR UPDATE OF d")
        .bind(node).fetch_all(&mut *connection).await?;
    for (kind, id, generation) in sources {
        sync_source_on(connection, secrets, &kind, &id, generation).await?;
    }
    Ok(())
}

/// Flush only sources created by one bounded node mutation. Other historical
/// dirty entries remain owned by the independent reference maintenance queue.
pub(crate) async fn flush_node_sources_on(
    connection: &mut PgConnection,
    secrets: &SecretBox,
    node: &str,
    histories: &[String],
) -> Result<(), sqlx::Error> {
    if histories.len() > 8 {
        return Err(sqlx::Error::Protocol(
            "too many owned history sources".into(),
        ));
    }
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(node)
        .fetch_optional(&mut *connection)
        .await?;
    let sources:Vec<(String,String,i64)>=sqlx::query_as("SELECT d.kind,d.source_id,d.generation FROM credential_reference_dirty d LEFT JOIN config_versions v ON d.kind='history' AND v.id=d.source_id WHERE (d.kind='node' AND d.source_id=$1) OR (d.kind='history' AND d.source_id=ANY($2::text[]) AND v.node_id=$1) ORDER BY d.kind,d.source_id FOR UPDATE OF d")
        .bind(node).bind(histories).fetch_all(&mut *connection).await?;
    for (kind, id, generation) in sources {
        sync_source_on(connection, secrets, &kind, &id, generation).await?;
    }
    Ok(())
}

async fn sync_source_on(
    connection: &mut PgConnection,
    secrets: &SecretBox,
    kind: &str,
    id: &str,
    generation: i64,
) -> Result<(), sqlx::Error> {
    let mut edges = Vec::<Value>::new();
    let mut resources = Vec::<Value>::new();
    let mut parents = std::collections::BTreeSet::<String>::new();
    let source = if kind == "node" {
        sqlx::query("SELECT id AS node_id,NULL::text AS config_version_id,NULL::bigint AS revision,desired_config_enc,deployed_config_enc,ssh_credential_id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
            .bind(id).fetch_optional(&mut *connection).await?
    } else {
        sqlx::query("SELECT node_id,id AS config_version_id,revision,config_enc AS desired_config_enc,NULL::text AS deployed_config_enc,NULL::text AS ssh_credential_id FROM config_versions WHERE id=$1 FOR UPDATE")
            .bind(id).fetch_optional(&mut *connection).await?
    };
    if let Some(row) = &source {
        if let Some(ssh) = row.get::<Option<String>, _>("ssh_credential_id") {
            parents.insert(ssh);
        }
        for (column, scope) in [
            (
                "desired_config_enc",
                if kind == "node" { "desired" } else { "history" },
            ),
            ("deployed_config_enc", "deployed"),
        ] {
            if let Some(cipher) = row.get::<Option<String>, _>(column) {
                let value: Value =
                    serde_json::from_str(&secrets.decrypt(&cipher).map_err(|_| {
                        sqlx::Error::Protocol(
                            "cannot decrypt configuration reference source".into(),
                        )
                    })?)
                    .map_err(|_| {
                        sqlx::Error::Protocol("invalid configuration reference source".into())
                    })?;
                for resource in crate::api::resources::collect_resource_ids(&value) {
                    resources.push(json!({"source":scope,"resource_id":resource}));
                }
                for (field, reference) in super::references(&value).map_err(|_| {
                    sqlx::Error::Protocol("invalid configuration reference source".into())
                })? {
                    parents.insert(reference.id.clone());
                    edges.push(json!({"source":scope,"field":field,"credential_id":reference.id,"version":reference.version}));
                }
            }
        }
    }
    let parents: Vec<_> = parents.into_iter().collect();
    sqlx::query("SELECT id FROM credentials WHERE id=ANY($1::text[]) ORDER BY id FOR KEY SHARE")
        .bind(&parents)
        .fetch_all(&mut *connection)
        .await?;
    let edges = Value::Array(edges);
    sqlx::query("SELECT v.credential_id,v.version FROM credential_versions v JOIN (SELECT DISTINCT credential_id,version FROM jsonb_to_recordset($1::jsonb) r(credential_id text,version bigint)) r ON r.credential_id=v.credential_id AND r.version=v.version ORDER BY v.credential_id,v.version FOR KEY SHARE OF v")
        .bind(&edges).fetch_all(&mut *connection).await?;
    let resources = Value::Array(resources);
    if let Some(row) = &source {
        sqlx::query("SELECT r.id FROM config_resources r JOIN (SELECT DISTINCT resource_id FROM jsonb_to_recordset($1::jsonb) e(resource_id text)) e ON e.resource_id=r.id WHERE r.node_id=$2 ORDER BY r.id FOR KEY SHARE OF r")
            .bind(&resources).bind(row.get::<String,_>("node_id")).fetch_all(&mut *connection).await?;
    }
    sqlx::query("DELETE FROM config_resource_references WHERE kind=$1 AND source_id=$2")
        .bind(kind)
        .bind(id)
        .execute(&mut *connection)
        .await?;
    sqlx::query("DELETE FROM credential_reference_edges WHERE kind=$1 AND source_id=$2")
        .bind(kind)
        .bind(id)
        .execute(&mut *connection)
        .await?;
    if let Some(row) = source {
        sqlx::query("INSERT INTO config_resource_references(kind,source_id,node_id,config_version_id,config_revision,source,resource_id) SELECT $1,$2,$3,$4,$5,source,resource_id FROM jsonb_to_recordset($6::jsonb) e(source text,resource_id text) ORDER BY resource_id,source")
            .bind(kind).bind(id).bind(row.get::<String,_>("node_id")).bind(row.get::<Option<String>,_>("config_version_id")).bind(row.get::<Option<i64>,_>("revision")).bind(&resources).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO credential_reference_edges(kind,source_id,node_id,config_version_id,config_revision,source,field,credential_id,version) SELECT $1,$2,$3,$4,$5,source,field,credential_id,version FROM jsonb_to_recordset($6::jsonb) r(source text,field text,credential_id text,version bigint) ORDER BY credential_id,version,source,field")
            .bind(kind).bind(id).bind(row.get::<String,_>("node_id")).bind(row.get::<Option<String>,_>("config_version_id")).bind(row.get::<Option<i64>,_>("revision")).bind(&edges).execute(&mut *connection).await?;
    }
    sqlx::query(
        "DELETE FROM credential_reference_dirty WHERE kind=$1 AND source_id=$2 AND generation=$3",
    )
    .bind(kind)
    .bind(id)
    .bind(generation)
    .execute(connection)
    .await?;
    Ok(())
}

/// A bounded maintenance batch. Parent rows precede source and queue rows.
async fn refresh_batch_on(
    connection: &mut PgConnection,
    secrets: &SecretBox,
) -> Result<usize, ApiError> {
    let candidates=sqlx::query("SELECT d.kind,d.source_id,CASE WHEN d.kind='node' THEN n.id ELSE v.node_id END AS node_id FROM credential_reference_dirty d LEFT JOIN nodes n ON d.kind='node' AND n.id=d.source_id LEFT JOIN config_versions v ON d.kind='history' AND v.id=d.source_id ORDER BY d.kind,d.source_id LIMIT 256")
        .fetch_all(&mut *connection).await?;
    let parents: std::collections::BTreeSet<String> = candidates
        .iter()
        .filter_map(|r| r.get::<Option<String>, _>("node_id"))
        .collect();
    let parents: Vec<_> = parents.into_iter().collect();
    let locked: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM nodes WHERE id=ANY($1::text[]) ORDER BY id FOR NO KEY UPDATE SKIP LOCKED",
    )
    .bind(parents)
    .fetch_all(&mut *connection)
    .await?;
    let selected:Vec<Value>=candidates.into_iter().filter(|r|r.get::<Option<String>,_>("node_id").is_none_or(|id|locked.contains(&id)))
        .map(|r|json!({"kind":r.get::<String,_>("kind"),"source_id":r.get::<String,_>("source_id")})).collect();
    let selected = Value::Array(selected);
    let sources:Vec<(String,String,i64)>=sqlx::query_as("SELECT d.kind,d.source_id,d.generation FROM credential_reference_dirty d JOIN jsonb_to_recordset($1::jsonb) c(kind text,source_id text) ON (c.kind,c.source_id)=(d.kind,d.source_id) ORDER BY d.kind,d.source_id FOR UPDATE OF d SKIP LOCKED")
        .bind(selected).fetch_all(&mut *connection).await?;
    let count = sources.len();
    for (kind, id, generation) in sources {
        sync_source_on(connection, secrets, &kind, &id, generation).await?;
    }
    Ok(count)
}
pub(crate) async fn flush_pending_on(
    connection: &mut PgConnection,
    secrets: &SecretBox,
) -> Result<(), ApiError> {
    let maintenance: bool = sqlx::query_scalar(
        "SELECT coalesce(current_setting('hysteriax.reference_maintenance',true),'off')='on'",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !maintenance {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "reference_maintenance_required",
            "legacy reference conversion requires explicit maintenance",
        ));
    }
    while refresh_batch_on(connection, secrets).await? > 0 {}
    assert_clean_on(connection).await
}

pub async fn initialize(pool: &PgPool, secrets: &SecretBox) -> Result<(), ApiError> {
    let maintenance: bool = sqlx::query_scalar(
        "SELECT coalesce(current_setting('hysteriax.reference_maintenance',true),'off')='on'",
    )
    .fetch_one(pool)
    .await?;
    if !maintenance {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "reference_maintenance_required",
            "reference initialization requires explicit maintenance",
        ));
    }
    loop {
        let secrets = secrets.clone();
        let count = db::write(pool, move |tx| {
            let secrets = secrets.clone();
            Box::pin(async move { refresh_batch_on(tx, &secrets).await })
        })
        .await?;
        let dirty: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_reference_dirty)")
                .fetch_one(pool)
                .await?;
        if !dirty {
            break;
        }
        if count == 0 {
            return Err(ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "reference_sources_busy",
                "configuration reference sources are busy; retry maintenance",
            ));
        }
    }
    // Initialization is an explicit maintenance operation, never an API read.
    for name in [
        "reference_edge_shape",
        "reference_edge_version",
        "reference_edge_history",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE credential_reference_edges VALIDATE CONSTRAINT {name}"
        )))
        .execute(pool)
        .await?;
    }
    assert_ready(pool).await
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
    async fn maintenance_upgrade_backfills_bounded_batches_without_changing_ciphertext() {
        let pool = crate::db::test_pool_through_version(2, 46).await;
        let secrets =
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        super::super::migration::migrate(&pool, &secrets)
            .await
            .unwrap();
        let node = "upgrade-node";
        let mut tx = pool.begin().await.unwrap();
        let credential = super::super::insert(
            &mut tx,
            &secrets,
            "Retained",
            "ssh_password",
            None,
            &json!({"secret":"preserved"}),
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap();
        let cipher = secrets
            .encrypt(
                &json!({"reference":format!("credential://{credential}/1/secret")}).to_string(),
            )
            .unwrap();
        sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_credential_id,ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES($1,'Upgrade','127.0.0.1',22,'root',$2,1,'example.test',443,':443','hash',$3,$3,$4,now(),now())")
            .bind(node).bind(&credential).bind(secrets.encrypt("retained-token").unwrap()).bind(&cipher).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) SELECT 'upgrade-'||n,$1,n,$2,'retained-hash',now() FROM generate_series(1,300) n").bind(node).bind(&cipher).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        let snapshot = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'history',(SELECT jsonb_agg(to_jsonb(v) ORDER BY id) FROM config_versions v),'credentials',(SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM credentials c),'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v))";
        let before: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        assert!(assert_ready(&pool).await.is_err());
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(refresh_batch_on(&mut tx, &secrets).await.unwrap(), 256);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credential_reference_dirty")
                .fetch_one(&mut *tx)
                .await
                .unwrap(),
            45
        );
        tx.rollback().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credential_reference_dirty")
                .fetch_one(&pool)
                .await
                .unwrap(),
            301
        );
        initialize(&pool, &secrets).await.unwrap();
        assert_ready(&pool).await.unwrap();
        let after: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
        assert_eq!(
            before, after,
            "maintenance must preserve every source row and encrypted byte"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM credential_reference_edges WHERE credential_id=$1"
            )
            .bind(&credential)
            .fetch_one(&pool)
            .await
            .unwrap(),
            301
        );
        initialize(&pool, &secrets).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, Value>(snapshot)
                .fetch_one(&pool)
                .await
                .unwrap(),
            before
        );
    }
    #[tokio::test]
    async fn runtime_reads_reject_uninitialized_sources_without_writing() {
        let pool = crate::db::test_pool().await;
        let secrets =
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        sqlx::query(
            "INSERT INTO credential_reference_dirty(kind,source_id) VALUES('node','missing')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let result = begin_read(&pool, &secrets).await;
        assert!(result.is_err());
        let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM credential_reference_dirty")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            pending, 1,
            "ordinary reads must not rebuild or acknowledge sources"
        );
        initialize(&pool, &secrets).await.unwrap();
        let mut tx = begin_read(&pool, &secrets).await.unwrap();
        let readonly: String = sqlx::query_scalar("SHOW transaction_read_only")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(readonly, "on");
        tx.commit().await.unwrap();
    }
    #[tokio::test]
    async fn changed_and_historical_references_block_deletion_without_prior_index_refresh() {
        let state = crate::state::AppState::new(
            crate::db::test_pool().await,
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
        );
        let mut ids = Vec::new();
        for name in ["First", "Second"] {
            let mut tx = state.pool.begin().await.unwrap();
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
            let mut source_tx = state.pool.begin().await.unwrap();
            sqlx::query("UPDATE nodes SET desired_config_enc=$1 WHERE id=$2")
                .bind(&cipher)
                .bind(node)
                .execute(&mut *source_tx)
                .await
                .unwrap();
            flush_node_on(&mut source_tx, &state.secrets, node)
                .await
                .unwrap();
            source_tx.commit().await.unwrap();
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
        let mut source_tx = state.pool.begin().await.unwrap();
        sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('history-test',$1,2,$2,'hash',now())").bind(node).bind(history).execute(&mut *source_tx).await.unwrap();
        flush_node_on(&mut source_tx, &state.secrets, node)
            .await
            .unwrap();
        source_tx.commit().await.unwrap();
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
